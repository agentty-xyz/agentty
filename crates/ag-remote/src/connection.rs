//! One client connection over a frame transport: authentication, pairing
//! approval, requests, and change pushes.

use std::sync::Arc;
use std::time::Duration;

use ag_noise::PublicKey;
use ag_session::{AnswerQuestionsRequest, QuestionAnswer, SessionError, SessionId, SessionService};
use tokio::sync::{oneshot, watch};
use tokio::time::{Instant, sleep_until, timeout};

use crate::PROTOCOL_VERSION;
use crate::device::{NewRemoteDevice, RemoteDevice, key_hash, token_hash};
use crate::entropy::random_hex;
use crate::pairing::random_code;
use crate::protocol::{
    ClientFrame, RemoteRequest, RemoteResponse, RemoteResult, RemoteSession, RemoteSessionSummary,
    ServerFrame,
};
use crate::server::{HandshakeSlot, RemoteConnection, ServerState};
use crate::transport::{CloseCause, FrameTransport, Incoming};

/// Time a client has to send its first frame.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Time the host has to approve or deny a pairing request.
pub(crate) const APPROVAL_TIMEOUT: Duration = Duration::from_mins(2);
/// Characters in the confirmation shown on the device and the terminal.
pub(crate) const CONFIRMATION_LENGTH: usize = 4;
/// Quiet period that coalesces bursts of host changes into one push.
pub(crate) const CHANGE_DEBOUNCE: Duration = Duration::from_millis(250);
/// Longest device name kept, in characters.
const MAX_DEVICE_NAME_CHARS: usize = 64;
/// Name used when a pairing device sends a blank one.
const DEFAULT_DEVICE_NAME: &str = "Remote device";
/// Rejection reason for device store failures, which stay out of the wire.
const DEVICE_STORE_UNAVAILABLE: &str = "Device store is unavailable";
/// Rejection reason when the host denies a pairing request.
const PAIRING_DENIED: &str = "Pairing was denied";
/// Rejection reason when the host does not decide in time.
const PAIRING_TIMED_OUT: &str = "Pairing was not approved in time";
/// Rejection reason for frames sent while pairing waits for approval.
const PAIRING_PENDING: &str = "Wait for pairing approval";
/// Close reason sent to every connection when the server stops.
const SERVER_STOPPED: &str = "Server stopped";

/// Runs one connection until the client leaves, the device is revoked, or
/// the server stops. `handshake` counts the connection as unauthenticated
/// until a device authenticates on it, and `proof` says how it does so.
pub(crate) async fn serve<T: FrameTransport>(
    mut transport: T,
    state: Arc<ServerState>,
    handshake: HandshakeSlot,
    proof: DeviceProof,
) {
    let _open_socket = state.track_socket();
    let mut revisions = state.revisions();
    let mut shutdown_rx = state.shutdown_rx.clone();

    let first_incoming = tokio::select! {
        biased;
        () = wait_until_stopping(&mut shutdown_rx) => {
            transport.close(CloseCause::Stopping, SERVER_STOPPED).await;

            return;
        }
        first_incoming = timeout(HANDSHAKE_TIMEOUT, transport.receive()) => first_incoming,
    };
    let Ok(first_incoming) = first_incoming else {
        reject(&mut transport, "Authentication timed out").await;

        return;
    };
    let Some(frame) = parse_text_frame(first_incoming) else {
        reject(&mut transport, "Expected a hello or pair frame").await;

        return;
    };
    let authenticated = match authenticate(&state, frame, &mut revisions, &proof).await {
        Ok(Authentication::Authenticated { connection, reply }) => Ok((connection, reply)),
        Ok(Authentication::PairingRequested(device)) => {
            let Some(decision) = await_approval(
                &mut transport,
                &state,
                device,
                &mut revisions,
                &mut shutdown_rx,
            )
            .await
            else {
                return;
            };

            decision
        }
        Err(reason) => Err(reason),
    };
    let (connection, reply) = match authenticated {
        Ok(authenticated) => authenticated,
        Err(reason) => {
            tracing::debug!(reason, "remote connection rejected");
            reject(&mut transport, reason).await;

            return;
        }
    };
    drop(handshake);
    let (close_rx, _registration) = state.register(connection);
    send(&mut transport, &reply).await;

    run(transport, &state, revisions, shutdown_rx, close_rx).await;
}

/// How a connection proves which device it is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeviceProof {
    /// The transport's encrypted channel was opened with this device key.
    /// The proof holds once the first frame has arrived over that channel.
    Key(PublicKey),
    /// The first frame carries the device token.
    Token,
}

/// Outcome of a valid first frame.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Authentication {
    /// A known device authenticated; register `connection` and send `reply`.
    Authenticated {
        /// Connection to register.
        connection: RemoteConnection,
        /// Frame that acknowledges the authentication.
        reply: ServerFrame,
    },
    /// A pairing code was redeemed; the device waits for host approval.
    PairingRequested(PairingDevice),
}

/// Credentials prepared for a device whose pairing code was redeemed.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct PairingDevice {
    /// Code the device and the terminal both show.
    pub(crate) confirmation: String,
    /// Digest stored for the device: of its token, or of its key.
    pub(crate) credential_hash: String,
    /// Identifier the device receives on approval.
    pub(crate) device_id: String,
    /// Bounded, trimmed device name.
    pub(crate) device_name: String,
    /// Secret token the device receives on approval; empty for a device
    /// that authenticates with its key.
    pub(crate) token: String,
}

/// Authenticates the first frame.
///
/// A known device marks the current host revision as reported. A redeemed
/// pairing code only prepares credentials; nothing is stored until the host
/// approves the request.
pub(crate) async fn authenticate(
    state: &ServerState,
    frame: ClientFrame,
    revisions: &mut watch::Receiver<u64>,
    proof: &DeviceProof,
) -> Result<Authentication, &'static str> {
    match frame {
        ClientFrame::Hello {
            protocol_version,
            token,
        } => {
            check_protocol_version(protocol_version)?;
            let credential_hash = match proof {
                DeviceProof::Key(public_key) => key_hash(public_key),
                DeviceProof::Token => token_hash(&token),
            };
            let device = find_device(state, &credential_hash)
                .await?
                .ok_or("Unknown device token")?;

            welcome(state, device, revisions).await
        }
        ClientFrame::Pair {
            code,
            device_name,
            protocol_version,
        } => {
            check_protocol_version(protocol_version)?;
            // A key device that already paired, for example after its
            // `Paired` frame was lost, is welcomed without spending the code.
            if let DeviceProof::Key(public_key) = proof
                && let Some(device) = find_device(state, &key_hash(public_key)).await?
            {
                return welcome(state, device, revisions).await;
            }
            let entropy = state.entropy.as_ref();
            let (Some(device_id), Some(token), Some(confirmation)) = (
                random_hex(entropy, 16),
                random_hex(entropy, 32),
                random_code(entropy, CONFIRMATION_LENGTH),
            ) else {
                return Err("Cannot generate device credentials");
            };
            if !state.pairing().redeem(&code) {
                return Err("Invalid or expired pairing code");
            }
            let (credential_hash, token) = match proof {
                DeviceProof::Key(public_key) => (key_hash(public_key), String::new()),
                DeviceProof::Token => (token_hash(&token), token),
            };

            Ok(Authentication::PairingRequested(PairingDevice {
                confirmation,
                credential_hash,
                device_id,
                device_name: device_display_name(&device_name),
                token,
            }))
        }
        ClientFrame::Request { .. } => Err("Authenticate before sending requests"),
    }
}

/// Finds the paired device stored under `credential_hash`.
async fn find_device(
    state: &ServerState,
    credential_hash: &str,
) -> Result<Option<RemoteDevice>, &'static str> {
    state
        .devices()
        .find_device_by_token_hash(credential_hash)
        .await
        .map_err(|_| DEVICE_STORE_UNAVAILABLE)
}

/// Records that `device` authenticated and returns its welcome, marking the
/// current host revision as reported.
async fn welcome(
    state: &ServerState,
    device: RemoteDevice,
    revisions: &mut watch::Receiver<u64>,
) -> Result<Authentication, &'static str> {
    state
        .devices()
        .touch_device(&device.id)
        .await
        .map_err(|_| DEVICE_STORE_UNAVAILABLE)?;
    let revision = *revisions.borrow_and_update();

    Ok(Authentication::Authenticated {
        connection: RemoteConnection {
            device_id: device.id.clone(),
            device_name: device.name,
        },
        reply: ServerFrame::Welcome {
            device_id: device.id,
            protocol_version: PROTOCOL_VERSION,
            revision,
        },
    })
}

/// Lists the pairing request, tells the device to wait, and returns the
/// authentication once the host approves.
///
/// Returns `None` when the device leaves or the server stops first; a
/// stopping server has already closed the transport.
async fn await_approval<T: FrameTransport>(
    transport: &mut T,
    state: &Arc<ServerState>,
    device: PairingDevice,
    revisions: &mut watch::Receiver<u64>,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> Option<Result<(RemoteConnection, ServerFrame), &'static str>> {
    let (decision_rx, _request) =
        state.request_pairing(device.device_name.clone(), device.confirmation.clone());
    let awaiting = ServerFrame::AwaitingApproval {
        confirmation: device.confirmation.clone(),
    };
    send(transport, &awaiting).await;
    let decision = tokio::select! {
        biased;
        () = wait_until_stopping(shutdown_rx) => {
            transport.close(CloseCause::Stopping, SERVER_STOPPED).await;

            return None;
        }
        decision = timeout(APPROVAL_TIMEOUT, decision_rx) => decision,
        incoming = transport.receive() => {
            return match incoming {
                Incoming::Closed => None,
                Incoming::Binary | Incoming::Control | Incoming::Text(_) => {
                    Some(Err(PAIRING_PENDING))
                }
            };
        }
    };

    Some(match decision {
        Ok(Ok(true)) => complete_pairing(state, device, revisions).await,
        Ok(Ok(false) | Err(_)) => Err(PAIRING_DENIED),
        Err(_) => Err(PAIRING_TIMED_OUT),
    })
}

/// Stores an approved device and returns the connection to register with
/// the frame that hands the device its token.
///
/// Marks the current host revision as reported.
pub(crate) async fn complete_pairing(
    state: &ServerState,
    device: PairingDevice,
    revisions: &mut watch::Receiver<u64>,
) -> Result<(RemoteConnection, ServerFrame), &'static str> {
    state
        .devices()
        .insert_device(NewRemoteDevice {
            id: device.device_id.clone(),
            name: device.device_name.clone(),
            token_hash: device.credential_hash,
        })
        .await
        .map_err(|_| DEVICE_STORE_UNAVAILABLE)?;
    tracing::info!(device_id = device.device_id, "remote device paired");
    let revision = *revisions.borrow_and_update();

    Ok((
        RemoteConnection {
            device_id: device.device_id.clone(),
            device_name: device.device_name,
        },
        ServerFrame::Paired {
            device_id: device.device_id,
            protocol_version: PROTOCOL_VERSION,
            revision,
            token: device.token,
        },
    ))
}

/// Serves requests and change pushes for one authenticated connection.
async fn run<T: FrameTransport>(
    mut transport: T,
    state: &ServerState,
    mut revisions: watch::Receiver<u64>,
    mut shutdown_rx: watch::Receiver<bool>,
    mut close_rx: oneshot::Receiver<()>,
) {
    let mut change_deadline: Option<Instant> = None;
    let mut revisions_open = true;

    loop {
        tokio::select! {
            biased;
            () = wait_until_stopping(&mut shutdown_rx) => {
                transport.close(CloseCause::Stopping, SERVER_STOPPED).await;

                return;
            }
            _ = &mut close_rx => {
                reject(&mut transport, "Device revoked").await;

                return;
            }
            () = sleep_until(change_deadline.unwrap_or_else(Instant::now)),
                if change_deadline.is_some() =>
            {
                change_deadline = None;
                let revision = *revisions.borrow_and_update();
                send(&mut transport, &ServerFrame::Changed { revision }).await;
            }
            changed = revisions.changed(), if revisions_open && change_deadline.is_none() => {
                if changed.is_ok() {
                    change_deadline = Some(Instant::now() + CHANGE_DEBOUNCE);
                } else {
                    revisions_open = false;
                }
            }
            incoming = transport.receive() => {
                match handle_incoming(state, incoming).await {
                    MessageOutcome::Closed => return,
                    MessageOutcome::Ignored => {}
                    MessageOutcome::Rejected(reason) => {
                        reject(&mut transport, reason).await;

                        return;
                    }
                    MessageOutcome::Reply(frame) => send(&mut transport, &frame).await,
                }
            }
        }
    }
}

/// What the connection does after one authenticated client message.
pub(crate) enum MessageOutcome {
    /// The client left or the transport failed.
    Closed,
    /// Control traffic that needs no reply.
    Ignored,
    /// The client broke the protocol; the connection closes.
    Rejected(&'static str),
    /// Send this frame and keep serving.
    Reply(ServerFrame),
}

/// Handles one thing received on an authenticated connection.
pub(crate) async fn handle_incoming(state: &ServerState, incoming: Incoming) -> MessageOutcome {
    match incoming {
        Incoming::Text(text) => match serde_json::from_str::<ClientFrame>(&text) {
            Ok(ClientFrame::Request { id, request }) => {
                MessageOutcome::Reply(ServerFrame::Response {
                    id,
                    result: execute(state.sessions(), request).await,
                })
            }
            Ok(ClientFrame::Hello { .. } | ClientFrame::Pair { .. }) => {
                MessageOutcome::Rejected("Connection is already authenticated")
            }
            Err(_) => MessageOutcome::Rejected("Invalid frame"),
        },
        Incoming::Binary => MessageOutcome::Rejected("Binary frames are not supported"),
        Incoming::Closed => MessageOutcome::Closed,
        Incoming::Control => MessageOutcome::Ignored,
    }
}

/// Runs one request against the session service.
async fn execute(sessions: &SessionService, request: RemoteRequest) -> RemoteResult {
    let result = match request {
        RemoteRequest::AnswerQuestions {
            answers,
            session_id,
        } => {
            let request = AnswerQuestionsRequest {
                answers: answers
                    .into_iter()
                    .map(|answer| QuestionAnswer {
                        answer: answer.answer,
                        question: answer.question,
                    })
                    .collect(),
                attachments: Vec::new(),
            };

            sessions
                .answer_questions(&SessionId::from(session_id), request)
                .await
                .map(|()| RemoteResponse::Accepted)
        }
        RemoteRequest::CancelSession { session_id } => sessions
            .cancel_session(&SessionId::from(session_id))
            .await
            .map(|()| RemoteResponse::Accepted),
        RemoteRequest::GetSession { session_id } => get_listed_session(sessions, session_id).await,
        RemoteRequest::ListSessions => {
            sessions
                .list_sessions()
                .await
                .map(|summaries| RemoteResponse::Sessions {
                    sessions: summaries
                        .into_iter()
                        .map(RemoteSessionSummary::from)
                        .collect(),
                })
        }
        RemoteRequest::SendMessage {
            message,
            session_id,
        } => sessions
            .send_message(&SessionId::from(session_id), message)
            .await
            .map(|()| RemoteResponse::Accepted),
    };

    match result {
        Ok(response) => RemoteResult::Ok(response),
        Err(error) => RemoteResult::Error(error.to_string()),
    }
}

/// Loads one session only when the host lists it, so remote clients read
/// nothing beyond the sessions they can see.
async fn get_listed_session(
    sessions: &SessionService,
    session_id: String,
) -> Result<RemoteResponse, SessionError> {
    let is_listed = sessions
        .list_sessions()
        .await?
        .iter()
        .any(|summary| summary.id.as_str() == session_id);
    if !is_listed {
        return Ok(RemoteResponse::Session { session: None });
    }
    let session = sessions.get_session(&SessionId::from(session_id)).await?;

    Ok(RemoteResponse::Session {
        session: session.map(RemoteSession::from),
    })
}

/// Resolves once the server starts stopping or its handle is gone.
async fn wait_until_stopping(shutdown_rx: &mut watch::Receiver<bool>) {
    let _ = shutdown_rx.wait_for(|is_stopping| *is_stopping).await;
}

/// Parses one text message as a client frame.
fn parse_text_frame(incoming: Incoming) -> Option<ClientFrame> {
    let Incoming::Text(text) = incoming else {
        return None;
    };

    serde_json::from_str(&text).ok()
}

/// Rejects frames from clients that speak another protocol version.
fn check_protocol_version(protocol_version: u32) -> Result<(), &'static str> {
    if protocol_version == PROTOCOL_VERSION {
        return Ok(());
    }

    Err("Unsupported protocol version")
}

/// Trims and bounds a client-chosen device name.
pub(crate) fn device_display_name(device_name: &str) -> String {
    let device_name: String = device_name
        .trim()
        .chars()
        .take(MAX_DEVICE_NAME_CHARS)
        .collect();
    if device_name.is_empty() {
        return DEFAULT_DEVICE_NAME.to_string();
    }

    device_name
}

/// Sends one frame.
async fn send<T: FrameTransport>(transport: &mut T, frame: &ServerFrame) {
    // Frames hold only strings, integers, and lists, so serialization
    // cannot fail.
    let text = serde_json::to_string(frame).unwrap_or_default();
    transport.send(text).await;
}

/// Sends a rejection frame, then closes the transport as rejected.
async fn reject<T: FrameTransport>(transport: &mut T, reason: &'static str) {
    send(
        transport,
        &ServerFrame::Rejected {
            reason: reason.to_string(),
        },
    )
    .await;
    transport.close(CloseCause::Rejected, reason).await;
}

#[cfg(test)]
#[path = "connection_test.rs"]
mod tests;
