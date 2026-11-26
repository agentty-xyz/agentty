use std::sync::Arc;

use ag_noise::PublicKey;
use ag_session::SessionOperation;
use tokio::sync::watch;

use super::{
    Authentication, DEFAULT_DEVICE_NAME, DEVICE_STORE_UNAVAILABLE, MAX_DEVICE_NAME_CHARS,
    MessageOutcome, PairingDevice, UNKNOWN_DEVICE, complete_pairing, device_display_name,
    handle_incoming,
};
use crate::PROTOCOL_VERSION;
use crate::device::key_hash;
use crate::protocol::{
    ClientFrame, RemoteGraphAction, RemoteGraphActionRequest, RemoteRequest, ServerFrame,
};
use crate::server::{RemoteConnection, ServerState};
use crate::source_support::{
    FIND, FailingEntropy, INSERT, MemoryDeviceStore, RecordingBackend, StateFixture, TOUCH,
};
use crate::transport::Incoming;

/// Authenticates the first frame of a connection opened with
/// [`device_key`].
async fn authenticate(
    state: &ServerState,
    frame: ClientFrame,
    revisions: &mut watch::Receiver<u64>,
) -> Result<Authentication, &'static str> {
    super::authenticate(state, frame, revisions, &device_key()).await
}

/// Public key of the device that connects in these tests.
fn device_key() -> PublicKey {
    PublicKey::from_bytes(&[7; 32]).expect("key should load")
}

/// A store that holds [`device_key`] as the device `phone`.
fn store_with_phone() -> MemoryDeviceStore {
    MemoryDeviceStore::default().with_device("phone", &key_hash(&device_key()))
}

/// Issues the code `AAAA-AAAA` that [`StateFixture::with_devices`] entropy
/// produces.
fn issue_code(fixture: &StateFixture) -> String {
    fixture
        .state
        .pairing()
        .issue(fixture.state.entropy.as_ref())
        .expect("replayed entropy should issue")
        .code
}

fn hello() -> ClientFrame {
    ClientFrame::Hello {
        protocol_version: PROTOCOL_VERSION,
    }
}

fn pair(code: &str, device_name: &str) -> ClientFrame {
    ClientFrame::Pair {
        code: code.to_string(),
        device_name: device_name.to_string(),
        protocol_version: PROTOCOL_VERSION,
    }
}

#[tokio::test]
async fn hello_from_a_paired_key_welcomes_and_touches_the_device() {
    // Arrange
    let fixture = StateFixture::with_devices(store_with_phone());
    let mut revisions = fixture.state.revisions();
    fixture.revisions_tx.send_replace(8);

    // Act
    let authenticated = authenticate(&fixture.state, hello(), &mut revisions).await;

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
async fn hello_rejects_unknown_keys_and_store_failures() {
    // Arrange
    let unknown = StateFixture::with_devices(MemoryDeviceStore::default());
    let other_key = StateFixture::with_devices(MemoryDeviceStore::default().with_device(
        "tablet",
        &key_hash(&PublicKey::from_bytes(&[8; 32]).expect("key should load")),
    ));
    let find_failure = StateFixture::with_devices(MemoryDeviceStore::failing(&[FIND]));
    let touch_failure = StateFixture::with_devices(
        MemoryDeviceStore::failing(&[TOUCH]).with_device("phone", &key_hash(&device_key())),
    );

    // Act
    let unknown_result =
        authenticate(&unknown.state, hello(), &mut unknown.state.revisions()).await;
    let other_key_result =
        authenticate(&other_key.state, hello(), &mut other_key.state.revisions()).await;
    let find_result = authenticate(
        &find_failure.state,
        hello(),
        &mut find_failure.state.revisions(),
    )
    .await;
    let touch_result = authenticate(
        &touch_failure.state,
        hello(),
        &mut touch_failure.state.revisions(),
    )
    .await;

    // Assert
    assert_eq!(unknown_result, Err("Unknown device token"));
    assert_eq!(other_key_result, Err(UNKNOWN_DEVICE));
    assert_eq!(find_result, Err(DEVICE_STORE_UNAVAILABLE));
    assert_eq!(touch_result, Err(DEVICE_STORE_UNAVAILABLE));
}

#[tokio::test]
async fn first_frames_reject_other_protocol_versions() {
    // Arrange
    let fixture = StateFixture::with_devices(MemoryDeviceStore::default());
    let code = issue_code(&fixture);
    let old_hello = ClientFrame::Hello {
        protocol_version: 0,
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

/// Builds the credentials [`StateFixture::with_devices`] entropy prepares
/// for `device_name` holding [`device_key`].
fn pairing_device(device_name: &str) -> PairingDevice {
    PairingDevice {
        confirmation: "AAAA".to_string(),
        credential_hash: key_hash(&device_key()),
        device_id: "00".repeat(16),
        device_name: device_name.to_string(),
    }
}

#[tokio::test]
async fn paired_device_that_pairs_again_is_welcomed_without_spending_the_code() {
    // Arrange
    let fixture = StateFixture::with_devices(store_with_phone());
    let code = issue_code(&fixture);

    // Act
    let repaired = authenticate(
        &fixture.state,
        pair(&code, "Phone"),
        &mut fixture.state.revisions(),
    )
    .await;

    // Assert
    assert!(matches!(repaired, Ok(Authentication::Authenticated { .. })));
    assert_eq!(fixture.devices.touched(), ["phone"]);
    assert!(fixture.state.pairing().redeem(&code));
}

#[tokio::test]
async fn pairing_reports_store_failures_without_spending_the_code() {
    // Arrange
    let fixture = StateFixture::with_devices(MemoryDeviceStore::failing(&[FIND]));
    let code = issue_code(&fixture);

    // Act
    let result = authenticate(
        &fixture.state,
        pair(&code, "Phone"),
        &mut fixture.state.revisions(),
    )
    .await;

    // Assert
    assert_eq!(result, Err(DEVICE_STORE_UNAVAILABLE));
    assert!(fixture.state.pairing().redeem(&code));
}

#[tokio::test]
async fn device_pairs_and_then_authenticates_with_its_key() {
    // Arrange
    let fixture = StateFixture::with_devices(MemoryDeviceStore::default());
    let code = issue_code(&fixture);
    let mut revisions = fixture.state.revisions();

    // Act
    let requested = authenticate(&fixture.state, pair(&code, "Phone"), &mut revisions).await;
    let device = match requested {
        Ok(Authentication::PairingRequested(device)) => Some(device),
        _ => None,
    }
    .expect("pairing should be requested");
    let paired = complete_pairing(&fixture.state, device, &mut revisions).await;
    let welcomed = authenticate(&fixture.state, hello(), &mut revisions).await;

    // Assert
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
async fn pair_with_valid_code_requests_approval_without_storing_a_device() {
    // Arrange
    let fixture = StateFixture::with_devices(MemoryDeviceStore::default());
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
async fn complete_pairing_stores_the_key_digest_and_sends_no_secret() {
    // Arrange
    let fixture = StateFixture::with_devices(MemoryDeviceStore::default());
    let mut revisions = fixture.state.revisions();
    fixture.revisions_tx.send_replace(8);

    // Act
    let paired = complete_pairing(&fixture.state, pairing_device("My phone"), &mut revisions).await;

    // Assert
    let device_id = "00".repeat(16);
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
            },
        ))
    );
    assert_eq!(
        fixture.devices.stored(),
        [(device_id, "My phone".to_string(), key_hash(&device_key()))]
    );
    assert!(!revisions.has_changed().expect("sender should be alive"));
}

#[tokio::test]
async fn complete_pairing_reports_store_failures() {
    // Arrange
    let fixture = StateFixture::with_devices(MemoryDeviceStore::failing(&[INSERT]));

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
    let wrong_code = StateFixture::with_devices(MemoryDeviceStore::default());
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
    let fixture = StateFixture::with_devices(MemoryDeviceStore::default());
    let frame = ClientFrame::Request {
        id: 1,
        request: RemoteRequest::ListSessions { project_id: None },
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

#[test]
fn handle_incoming_rejects_protocol_violations() {
    // Arrange
    let repeated_hello = serde_json::to_string(&hello()).expect("frame should encode");

    // Act
    let outcomes = [
        handle_incoming(Incoming::Text(repeated_hello)),
        handle_incoming(Incoming::Text("not json".to_string())),
        handle_incoming(Incoming::Binary),
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
    assert!(matches!(outcomes[3], MessageOutcome::Closed));
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
async fn authentication_lists_only_the_capabilities_the_host_serves() {
    // Arrange
    let fixture = StateFixture::supporting(store_with_phone(), &[SessionOperation::MergeSession]);
    let mut revisions = fixture.state.revisions();

    // Act
    let welcomed = authenticate(&fixture.state, hello(), &mut revisions).await;
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
async fn capabilities_follow_the_order_of_the_capability_table() {
    // Arrange
    let fixture = StateFixture::supporting(
        store_with_phone(),
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
    let welcomed = authenticate(&fixture.state, hello(), &mut revisions).await;

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
