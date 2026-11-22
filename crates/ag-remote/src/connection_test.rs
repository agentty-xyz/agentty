use std::sync::Arc;

use ag_noise::PublicKey;
use ag_session::SessionOperation;
use tokio::sync::watch;

use super::{
    Authentication, DEFAULT_DEVICE_NAME, DEVICE_STORE_UNAVAILABLE, DeviceProof,
    MAX_DEVICE_NAME_CHARS, MessageOutcome, PairingDevice, complete_pairing, device_display_name,
    execute, handle_incoming,
};
use crate::PROTOCOL_VERSION;
use crate::device::{key_hash, token_hash};
use crate::protocol::{
    ClientFrame, RemoteQuestionAnswer, RemoteRequest, RemoteResponse, RemoteResult, RemoteSession,
    RemoteSessionSummary, ServerFrame,
};
use crate::server::{RemoteConnection, ServerState};
use crate::test_support::{
    FIND, FailingEntropy, INSERT, MemoryDeviceStore, RecordingBackend, ReplayEntropy, StateFixture,
    TOUCH,
};
use crate::transport::Incoming;

/// State over `devices` with deterministic entropy and a succeeding backend.
fn fixture(devices: MemoryDeviceStore) -> StateFixture {
    StateFixture::new(
        devices,
        RecordingBackend::default(),
        Arc::new(ReplayEntropy::new(vec![0])),
    )
}

/// State with deterministic entropy over a backend that serves `operations`.
fn supporting_fixture(devices: MemoryDeviceStore, operations: &[SessionOperation]) -> StateFixture {
    StateFixture::new(
        devices,
        RecordingBackend::supporting(operations),
        Arc::new(ReplayEntropy::new(vec![0])),
    )
}

/// Authenticates the first frame of a connection that proves itself with a
/// token.
async fn authenticate(
    state: &ServerState,
    frame: ClientFrame,
    revisions: &mut watch::Receiver<u64>,
) -> Result<Authentication, &'static str> {
    super::authenticate(state, frame, revisions, &DeviceProof::Token).await
}

/// Public key of the device that connects through a relay in these tests.
fn device_key() -> PublicKey {
    PublicKey::from_bytes(&[7; 32]).expect("key should load")
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

fn request_message(request: RemoteRequest) -> Incoming {
    let frame = ClientFrame::Request { id: 9, request };

    Incoming::Text(serde_json::to_string(&frame).expect("frame should encode"))
}

/// Sends `request` as request `9` and returns the result of running it.
async fn reply_result(state: &ServerState, request: RemoteRequest) -> RemoteResult {
    let request = match handle_incoming(request_message(request)) {
        MessageOutcome::Request { id: 9, request } => Some(request),
        _ => None,
    }
    .expect("expected request 9");

    execute(state.sessions(), request).await
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
    assert_eq!(
        authenticated,
        Ok(Authentication::Authenticated {
            connection: RemoteConnection {
                device_id: "phone".to_string(),
                device_name: "phone name".to_string(),
            },
            reply: ServerFrame::Welcome {
                capabilities: Vec::new(),
                device_id: "phone".to_string(),
                protocol_version: PROTOCOL_VERSION,
                revision: 8,
            },
        })
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

/// Builds the credentials [`fixture`] entropy prepares for `device_name`.
fn pairing_device(device_name: &str) -> PairingDevice {
    PairingDevice {
        confirmation: "AAAA".to_string(),
        credential_hash: token_hash(&"00".repeat(32)),
        device_id: "00".repeat(16),
        device_name: device_name.to_string(),
        token: "00".repeat(32),
    }
}

#[tokio::test]
async fn paired_key_device_that_pairs_again_is_welcomed_without_spending_the_code() {
    // Arrange
    let fixture =
        fixture(MemoryDeviceStore::default().with_device("phone", &key_hash(&device_key())));
    let code = issue_code(&fixture);
    let proof = DeviceProof::Key(device_key());

    // Act
    let repaired = super::authenticate(
        &fixture.state,
        pair(&code, "Phone"),
        &mut fixture.state.revisions(),
        &proof,
    )
    .await;

    // Assert
    assert!(matches!(repaired, Ok(Authentication::Authenticated { .. })));
    assert_eq!(fixture.devices.touched(), ["phone"]);
    assert!(fixture.state.pairing().redeem(&code));
}

#[tokio::test]
async fn key_device_pairing_reports_store_failures() {
    // Arrange
    let fixture = fixture(MemoryDeviceStore::failing(&[FIND]));
    let code = issue_code(&fixture);

    // Act
    let result = super::authenticate(
        &fixture.state,
        pair(&code, "Phone"),
        &mut fixture.state.revisions(),
        &DeviceProof::Key(device_key()),
    )
    .await;

    // Assert
    assert_eq!(result, Err(DEVICE_STORE_UNAVAILABLE));
    assert!(fixture.state.pairing().redeem(&code));
}

#[tokio::test]
async fn key_device_pairs_and_authenticates_without_a_token() {
    // Arrange
    let fixture = fixture(MemoryDeviceStore::default());
    let code = issue_code(&fixture);
    let proof = DeviceProof::Key(device_key());
    let mut revisions = fixture.state.revisions();

    // Act
    let requested =
        super::authenticate(&fixture.state, pair(&code, "Phone"), &mut revisions, &proof).await;
    let device = match requested {
        Ok(Authentication::PairingRequested(device)) => Some(device),
        _ => None,
    }
    .expect("pairing should be requested");
    let token = device.token.clone();
    let paired = complete_pairing(&fixture.state, device, &mut revisions).await;
    let welcomed =
        super::authenticate(&fixture.state, hello("ignored"), &mut revisions, &proof).await;

    // Assert
    assert_eq!(token, "");
    assert!(paired.is_ok());
    assert_eq!(
        fixture.devices.stored(),
        [(
            "00".repeat(16),
            "Phone".to_string(),
            key_hash(&device_key())
        )]
    );
    assert!(matches!(welcomed, Ok(Authentication::Authenticated { .. })));
}

#[tokio::test]
async fn a_public_key_presented_as_a_token_authenticates_nothing() {
    // Arrange
    let fixture =
        fixture(MemoryDeviceStore::default().with_device("phone", &key_hash(&device_key())));
    let key_as_token = device_key().to_hex();
    let other_key = DeviceProof::Key(PublicKey::from_bytes(&[8; 32]).expect("key should load"));

    // Act
    let with_token = authenticate(
        &fixture.state,
        hello(&key_as_token),
        &mut fixture.state.revisions(),
    )
    .await;
    let with_other_key = super::authenticate(
        &fixture.state,
        hello(""),
        &mut fixture.state.revisions(),
        &other_key,
    )
    .await;

    // Assert
    assert_eq!(with_token, Err("Unknown device token"));
    assert_eq!(with_other_key, Err("Unknown device token"));
}

#[tokio::test]
async fn pair_with_valid_code_requests_approval_without_storing_a_device() {
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
    assert_eq!(
        authenticated,
        Ok(Authentication::PairingRequested(pairing_device("My phone")))
    );
    assert_eq!(fixture.devices.stored(), []);
}

#[tokio::test]
async fn complete_pairing_stores_only_the_token_hash() {
    // Arrange
    let fixture = fixture(MemoryDeviceStore::default());
    let mut revisions = fixture.state.revisions();
    fixture.revisions_tx.send_replace(8);

    // Act
    let paired = complete_pairing(&fixture.state, pairing_device("My phone"), &mut revisions).await;

    // Assert
    let device_id = "00".repeat(16);
    let token = "00".repeat(32);
    assert_eq!(
        paired,
        Ok((
            RemoteConnection {
                device_id: device_id.clone(),
                device_name: "My phone".to_string(),
            },
            ServerFrame::Paired {
                capabilities: Vec::new(),
                device_id: device_id.clone(),
                protocol_version: PROTOCOL_VERSION,
                revision: 8,
                token: token.clone(),
            },
        ))
    );
    assert_eq!(
        fixture.devices.stored(),
        [(device_id, "My phone".to_string(), token_hash(&token))]
    );
    assert!(!revisions.has_changed().expect("sender should be alive"));
}

#[tokio::test]
async fn complete_pairing_reports_store_failures() {
    // Arrange
    let fixture = fixture(MemoryDeviceStore::failing(&[INSERT]));

    // Act
    let paired = complete_pairing(
        &fixture.state,
        pairing_device("Phone"),
        &mut fixture.state.revisions(),
    )
    .await;

    // Assert
    assert_eq!(paired, Err(DEVICE_STORE_UNAVAILABLE));
    assert_eq!(fixture.devices.stored(), []);
}

#[tokio::test]
async fn pair_rejects_invalid_codes_and_missing_entropy() {
    // Arrange
    let wrong_code = fixture(MemoryDeviceStore::default());
    issue_code(&wrong_code);
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
    let entropy_result = authenticate(
        &no_entropy.state,
        pair("AAAA-AAAA", "Phone"),
        &mut no_entropy.state.revisions(),
    )
    .await;

    // Assert
    assert_eq!(wrong_result, Err("Invalid or expired pairing code"));
    assert_eq!(entropy_result, Err("Cannot generate device credentials"));
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
async fn handle_incoming_runs_each_request_against_the_session_service() {
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
        results.push(reply_result(&fixture.state, request).await);
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
            "list",
            "get:session-1",
            "list",
            "list",
            "send:session-1:hi",
            "list",
            "answer:session-1:Which branch?=main:0",
            "list",
            "cancel:session-1"
        ]
    );
}

#[tokio::test]
async fn handle_incoming_refuses_mutations_of_unlisted_sessions() {
    // Arrange
    let fixture = fixture(MemoryDeviceStore::default());
    let requests = vec![
        RemoteRequest::SendMessage {
            message: "hi".to_string(),
            session_id: "other-project".to_string(),
        },
        RemoteRequest::AnswerQuestions {
            answers: Vec::new(),
            session_id: "other-project".to_string(),
        },
        RemoteRequest::CancelSession {
            session_id: "other-project".to_string(),
        },
    ];

    // Act
    let mut results = Vec::new();
    for request in requests {
        results.push(reply_result(&fixture.state, request).await);
    }

    // Assert
    let not_found = RemoteResult::Error("Session not found".to_string());
    assert_eq!(results, [not_found.clone(), not_found.clone(), not_found]);
    assert_eq!(fixture.backend.calls(), ["list", "list", "list"]);
}

#[tokio::test]
async fn handle_incoming_reports_backend_errors_in_the_response() {
    // Arrange
    let fixture = StateFixture::new(
        MemoryDeviceStore::default(),
        RecordingBackend::failing(),
        Arc::new(ReplayEntropy::new(vec![0])),
    );

    // Act
    let listed = reply_result(&fixture.state, RemoteRequest::ListSessions).await;
    let loaded = reply_result(
        &fixture.state,
        RemoteRequest::GetSession {
            session_id: "session-1".to_string(),
        },
    )
    .await;

    // Assert
    assert_eq!(listed, RemoteResult::Error("backend failed".to_string()));
    assert_eq!(loaded, RemoteResult::Error("backend failed".to_string()));
    assert_eq!(fixture.backend.calls(), ["list", "list"]);
}

#[test]
fn handle_incoming_rejects_protocol_violations_and_ignores_control_frames() {
    // Arrange
    let repeated_hello = serde_json::to_string(&hello("t")).expect("frame should encode");

    // Act
    let outcomes = [
        handle_incoming(Incoming::Text(repeated_hello)),
        handle_incoming(Incoming::Text("not json".to_string())),
        handle_incoming(Incoming::Binary),
        handle_incoming(Incoming::Control),
        handle_incoming(Incoming::Closed),
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
    assert!(matches!(outcomes[4], MessageOutcome::Closed));
}

#[tokio::test]
async fn authentication_lists_only_the_capabilities_the_host_serves() {
    // Arrange
    let fixture = supporting_fixture(
        MemoryDeviceStore::default().with_device("phone", &token_hash("t")),
        &[SessionOperation::MergeSession],
    );
    let mut revisions = fixture.state.revisions();

    // Act
    let welcomed = authenticate(&fixture.state, hello("t"), &mut revisions).await;
    let paired = complete_pairing(&fixture.state, pairing_device("Phone"), &mut revisions).await;

    // Assert
    let welcome_capabilities = match welcomed {
        Ok(Authentication::Authenticated {
            reply: ServerFrame::Welcome { capabilities, .. },
            ..
        }) => Some(capabilities),
        _ => None,
    };
    let paired_capabilities = match paired {
        Ok((_, ServerFrame::Paired { capabilities, .. })) => Some(capabilities),
        _ => None,
    };
    let merge = vec!["merge".to_string()];
    assert_eq!(welcome_capabilities, Some(merge.clone()));
    assert_eq!(paired_capabilities, Some(merge));
}

#[tokio::test]
async fn merge_reaches_the_host_only_for_listed_sessions_it_serves() {
    // Arrange
    let serving = supporting_fixture(
        MemoryDeviceStore::default(),
        &[SessionOperation::MergeSession],
    );
    let not_serving = fixture(MemoryDeviceStore::default());
    let merge = |session_id: &str| RemoteRequest::MergeSession {
        session_id: session_id.to_string(),
    };

    // Act
    let merged = reply_result(&serving.state, merge("session-1")).await;
    let unlisted = reply_result(&serving.state, merge("other-project")).await;
    let unsupported = reply_result(&not_serving.state, merge("session-1")).await;

    // Assert
    assert_eq!(merged, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(serving.backend.calls(), ["list", "merge:session-1", "list"]);
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}
