use std::sync::Arc;

use axum::extract::ws::Message;

use super::{
    DEFAULT_DEVICE_NAME, DEVICE_STORE_UNAVAILABLE, MAX_DEVICE_NAME_CHARS, MessageOutcome,
    authenticate, device_display_name, handle_message,
};
use crate::PROTOCOL_VERSION;
use crate::device::token_hash;
use crate::protocol::{
    ClientFrame, RemoteQuestionAnswer, RemoteRequest, RemoteResponse, RemoteResult, RemoteSession,
    RemoteSessionSummary, ServerFrame,
};
use crate::server::RemoteConnection;
use crate::test_support::{
    FIND, FailingEntropy, INSERT, MemoryDeviceStore, RecordingBackend, ReplayEntropy, StateFixture,
    TOUCH,
};

/// State over `devices` with deterministic entropy and a succeeding backend.
fn fixture(devices: MemoryDeviceStore) -> StateFixture {
    StateFixture::new(
        devices,
        RecordingBackend::default(),
        Arc::new(ReplayEntropy::new(vec![0])),
    )
}

/// Issues the code `AAAA-AAAA` that [`fixture`] entropy produces.
fn issue_code(fixture: &StateFixture) -> String {
    fixture
        .state
        .pairing()
        .issue(fixture.state.entropy.as_ref())
        .expect("replayed entropy should issue")
        .code
}

fn hello(token: &str) -> ClientFrame {
    ClientFrame::Hello {
        protocol_version: PROTOCOL_VERSION,
        token: token.to_string(),
    }
}

fn pair(code: &str, device_name: &str) -> ClientFrame {
    ClientFrame::Pair {
        code: code.to_string(),
        device_name: device_name.to_string(),
        protocol_version: PROTOCOL_VERSION,
    }
}

fn request_message(request: RemoteRequest) -> Message {
    let frame = ClientFrame::Request { id: 9, request };

    Message::Text(
        serde_json::to_string(&frame)
            .expect("frame should encode")
            .into(),
    )
}

/// Returns the result of a response to request `9`.
fn reply_result(outcome: MessageOutcome) -> RemoteResult {
    let response = match outcome {
        MessageOutcome::Reply(ServerFrame::Response { id: 9, result }) => Some(result),
        _ => None,
    };

    response.expect("expected a response to request 9")
}

#[tokio::test]
async fn hello_with_known_token_welcomes_and_touches_device() {
    // Arrange
    let fixture = fixture(MemoryDeviceStore::default().with_device("phone", &token_hash("t")));
    let mut revisions = fixture.state.revisions();
    fixture.revisions_tx.send_replace(8);

    // Act
    let authenticated = authenticate(&fixture.state, hello("t"), &mut revisions).await;

    // Assert
    let (connection, reply) = authenticated.expect("known token should authenticate");
    assert_eq!(
        connection,
        RemoteConnection {
            device_id: "phone".to_string(),
            device_name: "phone name".to_string(),
        }
    );
    assert_eq!(
        reply,
        ServerFrame::Welcome {
            device_id: "phone".to_string(),
            protocol_version: PROTOCOL_VERSION,
            revision: 8,
        }
    );
    assert_eq!(fixture.devices.touched(), ["phone"]);
    assert!(!revisions.has_changed().expect("sender should be alive"));
}

#[tokio::test]
async fn hello_rejects_unknown_token_and_store_failures() {
    // Arrange
    let unknown = fixture(MemoryDeviceStore::default());
    let find_failure = fixture(MemoryDeviceStore::failing(&[FIND]));
    let touch_failure =
        fixture(MemoryDeviceStore::failing(&[TOUCH]).with_device("phone", &token_hash("t")));

    // Act
    let unknown_result =
        authenticate(&unknown.state, hello("t"), &mut unknown.state.revisions()).await;
    let find_result = authenticate(
        &find_failure.state,
        hello("t"),
        &mut find_failure.state.revisions(),
    )
    .await;
    let touch_result = authenticate(
        &touch_failure.state,
        hello("t"),
        &mut touch_failure.state.revisions(),
    )
    .await;

    // Assert
    assert_eq!(unknown_result, Err("Unknown device token"));
    assert_eq!(find_result, Err(DEVICE_STORE_UNAVAILABLE));
    assert_eq!(touch_result, Err(DEVICE_STORE_UNAVAILABLE));
}

#[tokio::test]
async fn first_frames_reject_other_protocol_versions() {
    // Arrange
    let fixture = fixture(MemoryDeviceStore::default());
    let code = issue_code(&fixture);
    let old_hello = ClientFrame::Hello {
        protocol_version: 0,
        token: "t".to_string(),
    };
    let new_pair = ClientFrame::Pair {
        code: code.clone(),
        device_name: "Phone".to_string(),
        protocol_version: PROTOCOL_VERSION + 1,
    };

    // Act
    let hello_result =
        authenticate(&fixture.state, old_hello, &mut fixture.state.revisions()).await;
    let pair_result = authenticate(&fixture.state, new_pair, &mut fixture.state.revisions()).await;

    // Assert
    assert_eq!(hello_result, Err("Unsupported protocol version"));
    assert_eq!(pair_result, Err("Unsupported protocol version"));
    assert!(fixture.state.pairing().redeem(&code));
}

#[tokio::test]
async fn pair_with_valid_code_stores_only_the_token_hash() {
    // Arrange
    let fixture = fixture(MemoryDeviceStore::default());
    let code = issue_code(&fixture);

    // Act
    let authenticated = authenticate(
        &fixture.state,
        pair(&code, "  My phone  "),
        &mut fixture.state.revisions(),
    )
    .await;

    // Assert
    let (connection, reply) = authenticated.expect("valid code should pair");
    let device_id = "00".repeat(16);
    let token = "00".repeat(32);
    assert_eq!(
        reply,
        ServerFrame::Paired {
            device_id: device_id.clone(),
            protocol_version: PROTOCOL_VERSION,
            revision: 7,
            token: token.clone(),
        }
    );
    assert_eq!(
        connection,
        RemoteConnection {
            device_id: device_id.clone(),
            device_name: "My phone".to_string(),
        }
    );
    assert_eq!(
        fixture.devices.stored(),
        [(device_id, "My phone".to_string(), token_hash(&token))]
    );
}

#[tokio::test]
async fn pair_rejects_invalid_codes_store_failures_and_missing_entropy() {
    // Arrange
    let wrong_code = fixture(MemoryDeviceStore::default());
    issue_code(&wrong_code);
    let insert_failure = fixture(MemoryDeviceStore::failing(&[INSERT]));
    let insert_code = issue_code(&insert_failure);
    let no_entropy = StateFixture::new(
        MemoryDeviceStore::default(),
        RecordingBackend::default(),
        Arc::new(FailingEntropy),
    );

    // Act
    let wrong_result = authenticate(
        &wrong_code.state,
        pair("ZZZZ-ZZZZ", "Phone"),
        &mut wrong_code.state.revisions(),
    )
    .await;
    let insert_result = authenticate(
        &insert_failure.state,
        pair(&insert_code, "Phone"),
        &mut insert_failure.state.revisions(),
    )
    .await;
    let entropy_result = authenticate(
        &no_entropy.state,
        pair("AAAA-AAAA", "Phone"),
        &mut no_entropy.state.revisions(),
    )
    .await;

    // Assert
    assert_eq!(wrong_result, Err("Invalid or expired pairing code"));
    assert_eq!(insert_result, Err(DEVICE_STORE_UNAVAILABLE));
    assert_eq!(entropy_result, Err("Cannot generate device credentials"));
    assert_eq!(insert_failure.devices.stored(), []);
}

#[tokio::test]
async fn requests_before_authentication_are_rejected() {
    // Arrange
    let fixture = fixture(MemoryDeviceStore::default());
    let frame = ClientFrame::Request {
        id: 1,
        request: RemoteRequest::ListSessions,
    };

    // Act
    let result = authenticate(&fixture.state, frame, &mut fixture.state.revisions()).await;

    // Assert
    assert_eq!(result, Err("Authenticate before sending requests"));
}

#[test]
fn device_display_name_trims_bounds_and_defaults() {
    // Arrange
    let long_name = "x".repeat(MAX_DEVICE_NAME_CHARS + 10);

    // Act
    let trimmed = device_display_name("  Phone \n");
    let bounded = device_display_name(&long_name);
    let blank = device_display_name("   ");

    // Assert
    assert_eq!(trimmed, "Phone");
    assert_eq!(bounded.chars().count(), MAX_DEVICE_NAME_CHARS);
    assert_eq!(blank, DEFAULT_DEVICE_NAME);
}

#[tokio::test]
async fn handle_message_runs_each_request_against_the_session_service() {
    // Arrange
    let fixture = fixture(MemoryDeviceStore::default());
    let requests = vec![
        RemoteRequest::ListSessions,
        RemoteRequest::GetSession {
            session_id: "session-1".to_string(),
        },
        RemoteRequest::GetSession {
            session_id: "missing".to_string(),
        },
        RemoteRequest::SendMessage {
            message: "hi".to_string(),
            session_id: "session-1".to_string(),
        },
        RemoteRequest::AnswerQuestions {
            answers: vec![RemoteQuestionAnswer {
                answer: "main".to_string(),
                question: "Which branch?".to_string(),
            }],
            session_id: "session-1".to_string(),
        },
        RemoteRequest::CancelSession {
            session_id: "session-1".to_string(),
        },
    ];

    // Act
    let mut results = Vec::new();
    for request in requests {
        results.push(reply_result(
            handle_message(&fixture.state, request_message(request)).await,
        ));
    }

    // Assert
    let session = crate::test_support::session_fixture(&"session-1".into());
    assert_eq!(
        results,
        [
            RemoteResult::Ok(RemoteResponse::Sessions {
                sessions: vec![RemoteSessionSummary {
                    id: "session-1".to_string(),
                    status: "InProgress".to_string(),
                    title: Some("Build it".to_string()),
                    updated_at: 20,
                }],
            }),
            RemoteResult::Ok(RemoteResponse::Session {
                session: Some(RemoteSession::from(session)),
            }),
            RemoteResult::Ok(RemoteResponse::Session { session: None }),
            RemoteResult::Ok(RemoteResponse::Accepted),
            RemoteResult::Ok(RemoteResponse::Accepted),
            RemoteResult::Ok(RemoteResponse::Accepted),
        ]
    );
    assert_eq!(
        fixture.backend.calls(),
        [
            "list",
            "get:session-1",
            "get:missing",
            "send:session-1:hi",
            "answer:session-1:Which branch?=main:0",
            "cancel:session-1"
        ]
    );
}

#[tokio::test]
async fn handle_message_reports_backend_errors_in_the_response() {
    // Arrange
    let fixture = StateFixture::new(
        MemoryDeviceStore::default(),
        RecordingBackend::failing(),
        Arc::new(ReplayEntropy::new(vec![0])),
    );

    // Act
    let result = reply_result(
        handle_message(&fixture.state, request_message(RemoteRequest::ListSessions)).await,
    );

    // Assert
    assert_eq!(result, RemoteResult::Error("backend failed".to_string()));
}

#[tokio::test]
async fn handle_message_rejects_protocol_violations_and_ignores_control_frames() {
    // Arrange
    let fixture = fixture(MemoryDeviceStore::default());
    let repeated_hello = serde_json::to_string(&hello("t")).expect("frame should encode");

    // Act
    let outcomes = [
        handle_message(&fixture.state, Message::Text(repeated_hello.into())).await,
        handle_message(&fixture.state, Message::Text("not json".into())).await,
        handle_message(&fixture.state, Message::Binary(vec![1].into())).await,
        handle_message(&fixture.state, Message::Ping(Vec::new().into())).await,
        handle_message(&fixture.state, Message::Pong(Vec::new().into())).await,
        handle_message(&fixture.state, Message::Close(None)).await,
    ];

    // Assert
    assert!(matches!(
        outcomes[0],
        MessageOutcome::Rejected("Connection is already authenticated")
    ));
    assert!(matches!(
        outcomes[1],
        MessageOutcome::Rejected("Invalid frame")
    ));
    assert!(matches!(
        outcomes[2],
        MessageOutcome::Rejected("Binary frames are not supported")
    ));
    assert!(matches!(outcomes[3], MessageOutcome::Ignored));
    assert!(matches!(outcomes[4], MessageOutcome::Ignored));
    assert!(matches!(outcomes[5], MessageOutcome::Closed));
    assert_eq!(fixture.backend.calls(), Vec::<String>::new());
}
