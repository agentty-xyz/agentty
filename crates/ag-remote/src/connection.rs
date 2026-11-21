//! One WebSocket connection: authentication, pairing approval, requests, and
//! change pushes.

use std::sync::Arc;
use std::time::Duration;

use ag_session::{AnswerQuestionsRequest, QuestionAnswer, SessionError, SessionId, SessionService};
use axum::extract::ws::{CloseFrame, Message, WebSocket, close_code};
use tokio::sync::{oneshot, watch};
use tokio::time::{Instant, sleep_until, timeout};

use crate::PROTOCOL_VERSION;
use crate::device::{NewRemoteDevice, token_hash};
use crate::entropy::random_hex;
use crate::pairing::random_code;
use crate::protocol::{
    ClientFrame, RemoteRequest, RemoteResponse, RemoteResult, RemoteSession, RemoteSessionSummary,
    ServerFrame,
};
use crate::server::{HandshakeSlot, RemoteConnection, ServerState};

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
/// Close reason sent to every socket when the server stops.
const SERVER_STOPPED: &str = "Server stopped";

/// Runs one upgraded socket until the client leaves, the device is revoked,
/// or the server stops. `handshake` counts the socket as unauthenticated
/// until a device authenticates on it.
pub(crate) async fn serve(
    mut socket: WebSocket,
    state: Arc<ServerState>,
    handshake: HandshakeSlot,
) {
    let _open_socket = state.track_socket();
    let mut revisions = state.revisions();
    let mut shutdown_rx = state.shutdown_rx.clone();

    let first_message = tokio::select! {
        biased;
        () = wait_until_stopping(&mut shutdown_rx) => {
            close(&mut socket, close_code::AWAY, SERVER_STOPPED).await;

            return;
        }
        first_message = timeout(HANDSHAKE_TIMEOUT, socket.recv()) => first_message,
    };
    let Ok(first_message) = first_message else {
        reject(&mut socket, "Authentication timed out").await;

        return;
    };
    let Some(frame) = first_message.and_then(|message| parse_text_frame(message.ok()?)) else {
        reject(&mut socket, "Expected a hello or pair frame").await;

        return;
    };
    let authenticated = match authenticate(&state, frame, &mut revisions).await {
        Ok(Authentication::Authenticated { connection, reply }) => Ok((connection, reply)),
        Ok(Authentication::PairingRequested(device)) => {
            let Some(decision) = await_approval(
                &mut socket,
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
            reject(&mut socket, reason).await;

            return;
        }
    };
    drop(handshake);
    let (close_rx, _registration) = state.register(connection);
    send(&mut socket, &reply).await;

    run(socket, &state, revisions, shutdown_rx, close_rx).await;
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
    /// Identifier the device receives on approval.
    pub(crate) device_id: String,
    /// Bounded, trimmed device name.
    pub(crate) device_name: String,
    /// Secret token the device receives on approval.
    pub(crate) token: String,
}

/// Authenticates the first frame.
///
/// Token authentication marks the current host revision as reported. A
/// redeemed pairing code only prepares credentials; nothing is stored until
/// the host approves the request.
pub(crate) async fn authenticate(
    state: &ServerState,
    frame: ClientFrame,
    revisions: &mut watch::Receiver<u64>,
) -> Result<Authentication, &'static str> {
    match frame {
        ClientFrame::Hello {
            protocol_version,
            token,
        } => {
            check_protocol_version(protocol_version)?;
            let device = state
                .devices()
                .find_device_by_token_hash(&token_hash(&token))
                .await
                .map_err(|_| DEVICE_STORE_UNAVAILABLE)?
                .ok_or("Unknown device token")?;
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
        ClientFrame::Pair {
            code,
            device_name,
            protocol_version,
        } => {
            check_protocol_version(protocol_version)?;
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

            Ok(Authentication::PairingRequested(PairingDevice {
                confirmation,
                device_id,
                device_name: device_display_name(&device_name),
                token,
            }))
        }
        ClientFrame::Request { .. } => Err("Authenticate before sending requests"),
    }
}

/// Lists the pairing request, tells the device to wait, and returns the
/// authentication once the host approves.
///
/// Returns `None` when the device leaves or the server stops first; a
/// stopping server has already closed the socket.
async fn await_approval(
    socket: &mut WebSocket,
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
    send(socket, &awaiting).await;
    let decision = tokio::select! {
        biased;
        () = wait_until_stopping(shutdown_rx) => {
            close(socket, close_code::AWAY, SERVER_STOPPED).await;

            return None;
        }
        decision = timeout(APPROVAL_TIMEOUT, decision_rx) => decision,
        message = socket.recv() => {
            return match message {
                Some(Ok(Message::Close(_)) | Err(_)) | None => None,
                Some(Ok(_)) => Some(Err(PAIRING_PENDING)),
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
            token_hash: token_hash(&device.token),
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
async fn run(
    mut socket: WebSocket,
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
                close(&mut socket, close_code::AWAY, SERVER_STOPPED).await;

                return;
            }
            _ = &mut close_rx => {
                reject(&mut socket, "Device revoked").await;

                return;
            }
            () = sleep_until(change_deadline.unwrap_or_else(Instant::now)),
                if change_deadline.is_some() =>
            {
                change_deadline = None;
                let revision = *revisions.borrow_and_update();
                send(&mut socket, &ServerFrame::Changed { revision }).await;
            }
            changed = revisions.changed(), if revisions_open && change_deadline.is_none() => {
                if changed.is_ok() {
                    change_deadline = Some(Instant::now() + CHANGE_DEBOUNCE);
                } else {
                    revisions_open = false;
                }
            }
            message = socket.recv() => {
                let Some(Ok(message)) = message else {
                    return;
                };
                match handle_message(state, message).await {
                    MessageOutcome::Closed => return,
                    MessageOutcome::Ignored => {}
                    MessageOutcome::Rejected(reason) => {
                        reject(&mut socket, reason).await;

                        return;
                    }
                    MessageOutcome::Reply(frame) => send(&mut socket, &frame).await,
                }
            }
        }
    }
}

/// What the connection does after one authenticated client message.
pub(crate) enum MessageOutcome {
    /// The client closed the socket.
    Closed,
    /// Control traffic that needs no reply.
    Ignored,
    /// The client broke the protocol; the connection closes.
    Rejected(&'static str),
    /// Send this frame and keep serving.
    Reply(ServerFrame),
}

/// Handles one message on an authenticated connection.
pub(crate) async fn handle_message(state: &ServerState, message: Message) -> MessageOutcome {
    match message {
        Message::Text(text) => match serde_json::from_str::<ClientFrame>(&text) {
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
        Message::Binary(_) => MessageOutcome::Rejected("Binary frames are not supported"),
        Message::Close(_) => MessageOutcome::Closed,
        Message::Ping(_) | Message::Pong(_) => MessageOutcome::Ignored,
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
fn parse_text_frame(message: Message) -> Option<ClientFrame> {
    let Message::Text(text) = message else {
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

/// Sends one frame. A failed send surfaces as the next receive failing.
async fn send(socket: &mut WebSocket, frame: &ServerFrame) {
    // Frames hold only strings, integers, and lists, so serialization
    // cannot fail.
    let text = serde_json::to_string(frame).unwrap_or_default();
    let _ = socket.send(Message::Text(text.into())).await;
}

/// Sends a rejection frame, then closes with a policy-violation code.
async fn reject(socket: &mut WebSocket, reason: &'static str) {
    send(
        socket,
        &ServerFrame::Rejected {
            reason: reason.to_string(),
        },
    )
    .await;
    close(socket, close_code::POLICY, reason).await;
}

/// Starts the closing handshake with `code` and `reason`.
async fn close(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
}

#[cfg(test)]
#[path = "connection_test.rs"]
mod tests;
