use std::sync::Arc;

use ag_noise::PublicKey;
use ag_session::{SessionDiffFile, SessionDiffFileStatus, SessionFileEntry, SessionOperation};
use tokio::sync::watch;

use super::{
    Authentication, DEFAULT_DEVICE_NAME, DEVICE_STORE_UNAVAILABLE, DeviceProof,
    MAX_DEVICE_NAME_CHARS, MessageOutcome, PairingDevice, complete_pairing, device_display_name,
    execute, handle_incoming, mutated_session,
};
use crate::PROTOCOL_VERSION;
use crate::device::{key_hash, token_hash};
use crate::protocol::{
    ClientFrame, RemoteDiffFile, RemoteDiffFileStatus, RemoteFileEntry, RemoteGraphAction,
    RemoteGraphActionRequest, RemoteModeModel, RemoteModelOption, RemoteProject, RemoteProjectSync,
    RemoteProjectSyncPhase, RemoteQuestionAnswer, RemoteRequest, RemoteResponse, RemoteResult,
    RemoteSession, RemoteSessionMode, RemoteSessionSummary, ServerFrame,
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

    execute(state.sessions(), state.entropy.as_ref(), request).await
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
            graph_question_target: None,
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
                project: None,
                sessions: vec![RemoteSessionSummary {
                    id: "session-1".to_string(),
                    status: "InProgress".to_string(),
                    title: Some("Build it".to_string()),
                    updated_at: 20,
                    ..RemoteSessionSummary::default()
                }],
            }),
            RemoteResult::Ok(RemoteResponse::Session {
                session: Some(Box::new(RemoteSession::from(session))),
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
            graph_question_target: None,
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

/// Returns one modified file whose patch is `text`.
fn diff_file(path: &str, text: &str) -> SessionDiffFile {
    SessionDiffFile {
        added_lines: 1,
        deleted_lines: 1,
        old_path: None,
        patch: text.to_string(),
        path: path.to_string(),
        status: SessionDiffFileStatus::Modified,
    }
}

/// Builds a diff request for `session_id`, naming `path` when given.
fn diff_request(session_id: &str, path: Option<&str>) -> RemoteRequest {
    RemoteRequest::GetSessionDiff {
        path: path.map(str::to_string),
        session_id: session_id.to_string(),
    }
}

#[tokio::test]
async fn session_diff_lists_files_and_returns_one_patch_for_listed_sessions() {
    // Arrange
    let patch = "diff --git a/a.rs b/a.rs\n-old\n+new\n";
    let serving = StateFixture::new(
        MemoryDeviceStore::default(),
        RecordingBackend::with_diff(Some(vec![diff_file("a.rs", patch)])),
        Arc::new(ReplayEntropy::new(vec![0])),
    );
    let not_serving = fixture(MemoryDeviceStore::default());
    let expected_file = RemoteDiffFile {
        added_lines: 1,
        deleted_lines: 1,
        old_path: None,
        path: "a.rs".to_string(),
        status: RemoteDiffFileStatus::Modified,
    };

    // Act
    let listed = reply_result(&serving.state, diff_request("session-1", None)).await;
    let opened = reply_result(&serving.state, diff_request("session-1", Some("a.rs"))).await;
    let absent = reply_result(&serving.state, diff_request("session-1", Some("b.rs"))).await;
    let unlisted = reply_result(&serving.state, diff_request("other-project", None)).await;
    let unsupported = reply_result(&not_serving.state, diff_request("session-1", None)).await;

    // Assert
    assert_eq!(
        listed,
        RemoteResult::Ok(RemoteResponse::Diff {
            files: vec![expected_file.clone()],
            patch: None,
            truncated: false,
        })
    );
    assert_eq!(
        opened,
        RemoteResult::Ok(RemoteResponse::Diff {
            files: vec![expected_file],
            patch: Some(patch.to_string()),
            truncated: false,
        })
    );
    assert_eq!(
        absent,
        RemoteResult::Error("File is not in the session diff".to_string())
    );
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(
        serving.backend.calls(),
        [
            "list",
            "diff:session-1",
            "list",
            "diff:session-1",
            "list",
            "diff:session-1",
            "list"
        ]
    );
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn session_graphs_are_read_for_listed_sessions_with_the_selection_asked_for() {
    // Arrange
    let graphs = ag_session::test_support::session_graphs("session-1");
    let serving = StateFixture::new(
        MemoryDeviceStore::default(),
        RecordingBackend::with_graphs(graphs.clone()),
        Arc::new(ReplayEntropy::new(vec![0])),
    );
    let not_serving = fixture(MemoryDeviceStore::default());
    let graphs_request =
        |session_id: &str, graph_id: Option<&str>| RemoteRequest::GetSessionGraphs {
            graph_id: graph_id.map(str::to_string),
            known: None,
            node_id: graph_id.map(|_| "fix".to_string()),
            session_id: session_id.to_string(),
        };

    // Act
    let shown = reply_result(&serving.state, graphs_request("session-1", None)).await;
    let selected = reply_result(&serving.state, graphs_request("session-1", Some("graph"))).await;
    let unlisted = reply_result(&serving.state, graphs_request("other-project", None)).await;
    let unsupported = reply_result(&not_serving.state, graphs_request("session-1", None)).await;

    // Assert
    let expected = RemoteResult::Ok(RemoteResponse::from_graphs(graphs));
    assert_eq!(shown, expected);
    assert_eq!(selected, expected);
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(
        serving.backend.calls(),
        [
            "list",
            "graphs:session-1:-:-",
            "list",
            "graphs:session-1:graph:fix",
            "list"
        ]
    );
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

/// `action` on node `fix` of `graph` in `session_id`, bound to the snapshot
/// `5eed`.
fn graph_action(session_id: &str, action: RemoteGraphAction) -> RemoteRequest {
    RemoteRequest::ActOnSessionGraph(RemoteGraphActionRequest {
        action,
        additional_calls: None,
        budget_id: None,
        graph_id: "graph".to_string(),
        message: None,
        node_id: Some("fix".to_string()),
        output_id: None,
        session_id: session_id.to_string(),
        snapshot: "5eed".to_string(),
    })
}

#[tokio::test]
async fn graph_actions_reach_only_listed_sessions_of_hosts_that_serve_them() {
    // Arrange
    let serving = supporting_fixture(
        MemoryDeviceStore::default(),
        &[SessionOperation::GraphActions],
    );
    let reading_only =
        supporting_fixture(MemoryDeviceStore::default(), &[SessionOperation::Graphs]);
    let retry = |session_id: &str| graph_action(session_id, RemoteGraphAction::Retry);
    let extend = |budget_id: Option<&str>| {
        RemoteRequest::ActOnSessionGraph(RemoteGraphActionRequest {
            action: RemoteGraphAction::ExtendBudget,
            additional_calls: Some(5),
            budget_id: budget_id.map(str::to_string),
            graph_id: "graph".to_string(),
            message: None,
            node_id: None,
            output_id: None,
            session_id: "session-1".to_string(),
            snapshot: "5eed".to_string(),
        })
    };

    // Act
    let retried = reply_result(&serving.state, retry("session-1")).await;
    let unlisted = reply_result(&serving.state, retry("other-project")).await;
    let unknown = reply_result(
        &serving.state,
        graph_action("session-1", RemoteGraphAction::Unsupported),
    )
    .await;
    let extended = reply_result(&serving.state, extend(Some("calls"))).await;
    let unnamed = reply_result(&serving.state, extend(None)).await;
    let unsupported = reply_result(&reading_only.state, retry("session-1")).await;

    // Assert
    assert_eq!(retried, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(extended, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        unnamed,
        RemoteResult::Error("Graph action needs `budget_id`".to_string())
    );
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(
        unknown,
        RemoteResult::Error("Unsupported graph action".to_string())
    );
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(
        serving.backend.calls(),
        [
            "list",
            "graph_action:session-1:graph:5eed:Retry { node_id: \"fix\" }",
            "list",
            "list",
            "list",
            "graph_action:session-1:graph:5eed:ExtendBudget { additional_calls: 5, budget_id: \
             \"calls\" }",
            "list"
        ]
    );
    assert_eq!(reading_only.backend.calls(), Vec::<String>::new());
}

/// Asks for a graph proposal for `message` in `session_id`.
fn graph_proposal(session_id: &str, message: &str) -> RemoteRequest {
    RemoteRequest::GenerateGraphProposal {
        message: message.to_string(),
        session_id: session_id.to_string(),
    }
}

#[tokio::test]
async fn graph_proposals_reach_only_listed_sessions_of_hosts_that_serve_them() {
    // Arrange
    let serving = supporting_fixture(
        MemoryDeviceStore::default(),
        &[SessionOperation::GenerateGraphProposal],
    );
    let acting_only = supporting_fixture(
        MemoryDeviceStore::default(),
        &[SessionOperation::GraphActions],
    );
    let no_entropy = StateFixture::new(
        MemoryDeviceStore::default(),
        RecordingBackend::supporting(&[SessionOperation::GenerateGraphProposal]),
        Arc::new(FailingEntropy),
    );
    let split = || graph_proposal("session-1", "Split the parser");

    // Act
    let generated = reply_result(&serving.state, split()).await;
    let unlisted = reply_result(
        &serving.state,
        graph_proposal("other-project", "Split the parser"),
    )
    .await;
    let unsupported = reply_result(&acting_only.state, split()).await;
    let without_identity = reply_result(&no_entropy.state, split()).await;

    // Assert
    assert_eq!(generated, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(
        without_identity,
        RemoteResult::Error("Cannot generate a graph proposal identity".to_string())
    );
    assert_eq!(
        serving.backend.calls(),
        [
            "list".to_string(),
            format!(
                "graph_proposal:session-1:remote-{}:Split the parser",
                "0".repeat(32)
            ),
            "list".to_string(),
        ]
    );
    assert_eq!(acting_only.backend.calls(), Vec::<String>::new());
    assert_eq!(no_entropy.backend.calls(), ["list"]);
}

#[tokio::test]
async fn graph_answers_reach_the_host_with_the_graph_work_they_answer() {
    // Arrange
    let fixture = fixture(MemoryDeviceStore::default());
    let older = r#"{"type":"request","id":9,"request":{"method":"answer_questions","answers":[{"answer":"Local","question":"Where?"}],"session_id":"session-1"}}"#;

    // Act
    let targeted = reply_result(
        &fixture.state,
        RemoteRequest::AnswerQuestions {
            answers: vec![RemoteQuestionAnswer {
                answer: "Local".to_string(),
                question: "Where?".to_string(),
            }],
            graph_question_target: Some("c0ffee".to_string()),
            session_id: "session-1".to_string(),
        },
    )
    .await;
    let request = match handle_incoming(Incoming::Text(older.to_string())) {
        MessageOutcome::Request { request, .. } => Some(request),
        _ => None,
    }
    .expect("an older answer frame should parse");
    let untargeted = execute(
        fixture.state.sessions(),
        fixture.state.entropy.as_ref(),
        request,
    )
    .await;

    // Assert
    assert_eq!(targeted, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(untargeted, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        fixture.backend.calls(),
        [
            "list",
            "answer:session-1:Where?=Local:0@c0ffee",
            "list",
            "answer:session-1:Where?=Local:0"
        ]
    );
}

#[test]
fn a_graph_proposal_queues_behind_earlier_changes_of_its_session() {
    // Arrange
    let request = graph_proposal("session-1", "Split the parser");

    // Act
    let session = mutated_session(&request);

    // Assert
    assert_eq!(session, Some("session-1"));
}

#[test]
fn an_unknown_graph_action_keeps_the_connection() {
    // Arrange
    let text = r#"{"type":"request","id":4,"request":{"method":"act_on_session_graph","action":"rewrite_history","graph_id":"graph","session_id":"session-1","snapshot":"5eed"}}"#;

    // Act
    let outcome = handle_incoming(Incoming::Text(text.to_string()));

    // Assert
    assert!(matches!(
        outcome,
        MessageOutcome::Request {
            id: 4,
            request: RemoteRequest::ActOnSessionGraph(RemoteGraphActionRequest {
                action: RemoteGraphAction::Unsupported,
                ..
            }),
        }
    ));
}

#[tokio::test]
async fn session_graphs_answer_unchanged_only_for_the_version_the_device_shows() {
    // Arrange
    let graphs = ag_session::test_support::session_graphs("session-1");
    let fixture = StateFixture::new(
        MemoryDeviceStore::default(),
        RecordingBackend::with_graphs(graphs.clone()),
        Arc::new(ReplayEntropy::new(vec![0])),
    );
    let full = RemoteResponse::from_graphs(graphs);
    let version = match &full {
        RemoteResponse::Graphs { version, .. } => version.clone(),
        _ => String::new(),
    };
    let graphs_request = |known: Option<&str>| RemoteRequest::GetSessionGraphs {
        graph_id: None,
        known: known.map(str::to_string),
        node_id: None,
        session_id: "session-1".to_string(),
    };

    // Act
    let unchanged = reply_result(&fixture.state, graphs_request(Some(&version))).await;
    let stale = reply_result(&fixture.state, graphs_request(Some("stale"))).await;
    let fresh = reply_result(&fixture.state, graphs_request(None)).await;

    // Assert
    assert_ne!(version, "");
    assert_eq!(
        unchanged,
        RemoteResult::Ok(RemoteResponse::Graphs {
            graph: None,
            graphs: Vec::new(),
            proposal: None,
            unchanged: true,
            version,
        })
    );
    assert_eq!(stale, RemoteResult::Ok(full.clone()));
    assert_eq!(fresh, RemoteResult::Ok(full));
}

#[tokio::test]
async fn session_diff_of_a_session_the_host_no_longer_loads_is_not_found() {
    // Arrange
    let fixture = StateFixture::new(
        MemoryDeviceStore::default(),
        RecordingBackend::with_diff(None),
        Arc::new(ReplayEntropy::new(vec![0])),
    );

    // Act
    let result = reply_result(&fixture.state, diff_request("session-1", None)).await;

    // Assert
    assert_eq!(result, RemoteResult::Error("Session not found".to_string()));
}

/// Builds a file-listing request for `session_id`.
fn files_request(session_id: &str) -> RemoteRequest {
    RemoteRequest::ListSessionFiles {
        session_id: session_id.to_string(),
    }
}

#[tokio::test]
async fn session_files_list_the_worktree_of_listed_sessions_only() {
    // Arrange
    let serving = StateFixture::new(
        MemoryDeviceStore::default(),
        RecordingBackend::with_files(Some(vec![
            SessionFileEntry {
                is_dir: true,
                path: "src".to_string(),
            },
            SessionFileEntry {
                is_dir: false,
                path: "src/lib.rs".to_string(),
            },
        ])),
        Arc::new(ReplayEntropy::new(vec![0])),
    );
    let not_serving = fixture(MemoryDeviceStore::default());

    // Act
    let listed = reply_result(&serving.state, files_request("session-1")).await;
    let unlisted = reply_result(&serving.state, files_request("other-project")).await;
    let unsupported = reply_result(&not_serving.state, files_request("session-1")).await;

    // Assert
    assert_eq!(
        listed,
        RemoteResult::Ok(RemoteResponse::SessionFiles {
            files: vec![
                RemoteFileEntry {
                    is_dir: true,
                    path: "src".to_string(),
                },
                RemoteFileEntry {
                    is_dir: false,
                    path: "src/lib.rs".to_string(),
                },
            ],
            truncated: false,
        })
    );
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(serving.backend.calls(), ["list", "files:session-1", "list"]);
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn session_files_of_a_session_the_host_no_longer_loads_are_not_found() {
    // Arrange
    let fixture = StateFixture::new(
        MemoryDeviceStore::default(),
        RecordingBackend::with_files(None),
        Arc::new(ReplayEntropy::new(vec![0])),
    );

    // Act
    let result = reply_result(&fixture.state, files_request("session-1")).await;

    // Assert
    assert_eq!(result, RemoteResult::Error("Session not found".to_string()));
}

#[tokio::test]
async fn project_files_list_the_project_checkout_only_when_the_host_serves_them() {
    // Arrange
    let serving = StateFixture::new(
        MemoryDeviceStore::default(),
        RecordingBackend::with_project_files(vec![SessionFileEntry {
            is_dir: false,
            path: "README.md".to_string(),
        }]),
        Arc::new(ReplayEntropy::new(vec![0])),
    );
    let not_serving = fixture(MemoryDeviceStore::default());

    // Act
    let listed = reply_result(&serving.state, RemoteRequest::ListProjectFiles).await;
    let unsupported = reply_result(&not_serving.state, RemoteRequest::ListProjectFiles).await;

    // Assert
    assert_eq!(
        listed,
        RemoteResult::Ok(RemoteResponse::SessionFiles {
            files: vec![RemoteFileEntry {
                is_dir: false,
                path: "README.md".to_string(),
            }],
            truncated: false,
        })
    );
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(serving.backend.calls(), ["project_files"]);
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn get_host_describes_the_project_and_only_remote_modes() {
    // Arrange
    let serving = supporting_fixture(
        MemoryDeviceStore::default(),
        &[SessionOperation::HostContext],
    );
    let not_serving = fixture(MemoryDeviceStore::default());

    // Act
    let host = reply_result(&serving.state, RemoteRequest::GetHost).await;
    let unsupported = reply_result(&not_serving.state, RemoteRequest::GetHost).await;

    // Assert
    assert_eq!(
        host,
        RemoteResult::Ok(RemoteResponse::Host {
            base_branch: "main".to_string(),
            default_model: "gpt-6.1-sol".to_string(),
            mode_models: vec![RemoteModeModel {
                agent: "codex".to_string(),
                mode: RemoteSessionMode::Regular,
                model: "gpt-6.1-sol".to_string(),
                name: "gpt-6.1-sol".to_string(),
                reasoning: Some("high".to_string()),
                response_style: Some("balanced".to_string()),
                speed_mode: Some("fast".to_string()),
            }],
            models: vec![RemoteModelOption::from(
                ag_session::test_support::codex_selection()
            )],
            project: "agentty".to_string(),
            session_modes: vec![RemoteSessionMode::Regular],
        })
    );
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(serving.backend.calls(), ["host"]);
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn create_session_uses_the_active_project_and_a_served_mode() {
    // Arrange
    let serving = supporting_fixture(
        MemoryDeviceStore::default(),
        &[SessionOperation::CreateSession],
    );
    let not_serving = fixture(MemoryDeviceStore::default());
    let create = |mode| RemoteRequest::CreateSession { mode };

    // Act
    let created = reply_result(&serving.state, create(RemoteSessionMode::Regular)).await;
    let harness = reply_result(&serving.state, create(RemoteSessionMode::Harness)).await;
    let unsupported = reply_result(&not_serving.state, create(RemoteSessionMode::Regular)).await;

    // Assert
    assert_eq!(
        created,
        RemoteResult::Ok(RemoteResponse::Created {
            session_id: "created".to_string(),
        })
    );
    assert_eq!(
        harness,
        RemoteResult::Error("Session mode is not available".to_string())
    );
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(
        serving.backend.calls(),
        ["host", "create:Regular:1", "host"]
    );
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn stop_turn_reaches_the_host_only_for_listed_sessions_it_serves() {
    // Arrange
    let serving = supporting_fixture(MemoryDeviceStore::default(), &[SessionOperation::StopTurn]);
    let not_serving = fixture(MemoryDeviceStore::default());
    let stop = |session_id: &str| RemoteRequest::StopTurn {
        session_id: session_id.to_string(),
    };

    // Act
    let stopped = reply_result(&serving.state, stop("session-1")).await;
    let unlisted = reply_result(&serving.state, stop("other-project")).await;
    let unsupported = reply_result(&not_serving.state, stop("session-1")).await;

    // Assert
    assert_eq!(stopped, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(serving.backend.calls(), ["list", "stop:session-1", "list"]);
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn sync_session_reaches_the_host_only_for_listed_sessions_it_serves() {
    // Arrange
    let serving = supporting_fixture(
        MemoryDeviceStore::default(),
        &[SessionOperation::RebaseSession],
    );
    let not_serving = fixture(MemoryDeviceStore::default());
    let sync = |session_id: &str| RemoteRequest::SyncSession {
        session_id: session_id.to_string(),
    };

    // Act
    let synced = reply_result(&serving.state, sync("session-1")).await;
    let unlisted = reply_result(&serving.state, sync("other-project")).await;
    let unsupported = reply_result(&not_serving.state, sync("session-1")).await;

    // Assert
    assert_eq!(synced, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(serving.backend.calls(), ["list", "sync:session-1", "list"]);
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn settings_reach_the_host_only_as_a_valid_change_to_a_listed_session() {
    // Arrange
    let serving = supporting_fixture(
        MemoryDeviceStore::default(),
        &[SessionOperation::SessionSettings],
    );
    let not_serving = fixture(MemoryDeviceStore::default());
    let codex_model = ag_session::test_support::CODEX_MODEL.as_str();
    let settings =
        |session_id: &str, model: &str, reasoning: &str| RemoteRequest::SetSessionSettings {
            model: Some(model.to_string()),
            reasoning: Some(reasoning.to_string()),
            response_style: Some("concise".to_string()),
            session_id: session_id.to_string(),
            speed_mode: Some("fast".to_string()),
        };
    let reasoning_only = RemoteRequest::SetSessionSettings {
        model: None,
        reasoning: Some("low".to_string()),
        response_style: None,
        session_id: "session-1".to_string(),
        speed_mode: None,
    };

    // Act
    let changed = reply_result(&serving.state, settings("session-1", codex_model, "max")).await;
    let partial = reply_result(&serving.state, reasoning_only).await;
    let unknown_model = reply_result(&serving.state, settings("session-1", "vim", "max")).await;
    let unknown_level =
        reply_result(&serving.state, settings("session-1", codex_model, "huge")).await;
    let unlisted = reply_result(
        &serving.state,
        settings("other-project", codex_model, "max"),
    )
    .await;
    let unsupported = reply_result(
        &not_serving.state,
        settings("session-1", codex_model, "max"),
    )
    .await;

    // Assert
    assert_eq!(changed, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(partial, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        unknown_model,
        RemoteResult::Error("unknown model: vim".to_string())
    );
    assert_eq!(
        unknown_level,
        RemoteResult::Error("unknown reasoning level: huge".to_string())
    );
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(
        serving.backend.calls(),
        [
            "list".to_string(),
            format!("settings:session-1:{codex_model}:max:concise:fast"),
            "list".to_string(),
            "settings:session-1:-:low:-:-".to_string(),
            "list".to_string(),
        ]
    );
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn capabilities_follow_the_order_of_the_capability_table() {
    // Arrange
    let fixture = supporting_fixture(
        MemoryDeviceStore::default().with_device("phone", &token_hash("t")),
        &[
            SessionOperation::GenerateGraphProposal,
            SessionOperation::GraphActions,
            SessionOperation::RebaseSession,
            SessionOperation::StopTurn,
            SessionOperation::Graphs,
            SessionOperation::MergeSession,
        ],
    );
    let mut revisions = fixture.state.revisions();

    // Act
    let welcomed = authenticate(&fixture.state, hello("t"), &mut revisions).await;

    // Assert
    let capabilities = match welcomed {
        Ok(Authentication::Authenticated {
            reply: ServerFrame::Welcome { capabilities, .. },
            ..
        }) => Some(capabilities),
        _ => None,
    };
    assert_eq!(
        capabilities,
        Some(vec![
            "merge".to_string(),
            "stop_turn".to_string(),
            "sync".to_string(),
            "graphs".to_string(),
            "graph_actions".to_string(),
            "graph_proposal".to_string()
        ])
    );
}

#[tokio::test]
async fn publish_returns_host_refusals_and_needs_the_host_to_serve_it() {
    // Arrange
    let serving = supporting_fixture(
        MemoryDeviceStore::default(),
        &[SessionOperation::PublishSession],
    );
    let not_serving = fixture(MemoryDeviceStore::default());
    let publish = || RemoteRequest::PublishSession {
        session_id: "session-1".to_string(),
    };

    // Act
    let refused = reply_result(&serving.state, publish()).await;
    let unsupported = reply_result(&not_serving.state, publish()).await;

    // Assert
    assert_eq!(refused, RemoteResult::Error("unsupported".to_string()));
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(serving.backend.calls(), ["list", "review:session-1"]);
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn sync_project_reaches_the_host_only_when_the_host_serves_it() {
    // Arrange
    let serving = supporting_fixture(
        MemoryDeviceStore::default(),
        &[SessionOperation::SyncProject],
    );
    let not_serving = fixture(MemoryDeviceStore::default());

    // Act
    let synced = reply_result(&serving.state, RemoteRequest::SyncProject).await;
    let unsupported = reply_result(&not_serving.state, RemoteRequest::SyncProject).await;

    // Assert
    assert_eq!(synced, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        unsupported,
        RemoteResult::Error("Unsupported request".to_string())
    );
    assert_eq!(serving.backend.calls(), ["sync_project"]);
    assert_eq!(not_serving.backend.calls(), Vec::<String>::new());
}

#[tokio::test]
async fn list_sessions_describes_the_project_when_the_host_reports_it() {
    // Arrange
    let reporting = supporting_fixture(
        MemoryDeviceStore::default(),
        &[SessionOperation::ProjectStatus],
    );

    // Act
    let listed = reply_result(&reporting.state, RemoteRequest::ListSessions).await;

    // Assert
    let project = match listed {
        RemoteResult::Ok(RemoteResponse::Sessions { project, .. }) => project,
        _ => None,
    };
    assert_eq!(
        project,
        Some(RemoteProject {
            ahead: Some(1),
            behind: Some(2),
            branch: Some("main".to_string()),
            name: "agentty".to_string(),
            sync: Some(RemoteProjectSync {
                branch: "main".to_string(),
                conflicts: Some(3),
                message: None,
                phase: RemoteProjectSyncPhase::ResolvingConflicts,
                pulled_commits: None,
                pushed_commits: None,
            }),
        })
    );
    assert_eq!(reporting.backend.calls(), ["list", "project"]);
}
