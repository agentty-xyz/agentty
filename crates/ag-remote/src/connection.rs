//! One WebSocket connection: authentication, requests, and change pushes.

use std::sync::Arc;
use std::time::Duration;

use ag_session::{AnswerQuestionsRequest, QuestionAnswer, SessionId, SessionService};
use axum::extract::ws::{CloseFrame, Message, WebSocket, close_code};
use tokio::sync::{oneshot, watch};
use tokio::time::{Instant, sleep_until, timeout};

use crate::PROTOCOL_VERSION;
use crate::device::{NewRemoteDevice, token_hash};
use crate::entropy::random_hex;
use crate::protocol::{
    ClientFrame, RemoteRequest, RemoteResponse, RemoteResult, RemoteSession, RemoteSessionSummary,
    ServerFrame,
};
use crate::server::{RemoteConnection, ServerState};

/// Time a client has to send its first frame.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Quiet period that coalesces bursts of host changes into one push.
pub(crate) const CHANGE_DEBOUNCE: Duration = Duration::from_millis(250);
/// Longest device name kept, in characters.
const MAX_DEVICE_NAME_CHARS: usize = 64;
/// Name used when a pairing device sends a blank one.
const DEFAULT_DEVICE_NAME: &str = "Remote device";
/// Rejection reason for device store failures, which stay out of the wire.
const DEVICE_STORE_UNAVAILABLE: &str = "Device store is unavailable";
/// Close reason sent to every socket when the server stops.
const SERVER_STOPPED: &str = "Server stopped";

/// Runs one upgraded socket until the client leaves, the device is revoked,
/// or the server stops.
pub(crate) async fn serve(mut socket: WebSocket, state: Arc<ServerState>) {
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
    let (connection, reply) = match authenticate(&state, frame, &mut revisions).await {
        Ok(authenticated) => authenticated,
        Err(reason) => {
            tracing::debug!(reason, "remote connection rejected");
            reject(&mut socket, reason).await;

            return;
        }
    };
    let (close_rx, _registration) = state.register(connection);
    send(&mut socket, &reply).await;

    run(socket, &state, revisions, shutdown_rx, close_rx).await;
}

/// Authenticates the first frame and returns the connection to register with
/// the frame that acknowledges it.
///
/// Successful authentication marks the current host revision as reported.
pub(crate) async fn authenticate(
    state: &ServerState,
    frame: ClientFrame,
    revisions: &mut watch::Receiver<u64>,
) -> Result<(RemoteConnection, ServerFrame), &'static str> {
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

            Ok((
                RemoteConnection {
                    device_id: device.id.clone(),
                    device_name: device.name,
                },
                ServerFrame::Welcome {
                    device_id: device.id,
                    protocol_version: PROTOCOL_VERSION,
                    revision,
                },
            ))
        }
        ClientFrame::Pair {
            code,
            device_name,
            protocol_version,
        } => {
            check_protocol_version(protocol_version)?;
            let entropy = state.entropy.as_ref();
            let (Some(device_id), Some(token)) = (random_hex(entropy, 16), random_hex(entropy, 32))
            else {
                return Err("Cannot generate device credentials");
            };
            if !state.pairing().redeem(&code) {
                return Err("Invalid or expired pairing code");
            }
            let device_name = device_display_name(&device_name);
            state
                .devices()
                .insert_device(NewRemoteDevice {
                    id: device_id.clone(),
                    name: device_name.clone(),
                    token_hash: token_hash(&token),
                })
                .await
                .map_err(|_| DEVICE_STORE_UNAVAILABLE)?;
            tracing::info!(device_id, "remote device paired");
            let revision = *revisions.borrow_and_update();

            Ok((
                RemoteConnection {
                    device_id: device_id.clone(),
                    device_name,
                },
                ServerFrame::Paired {
                    device_id,
                    protocol_version: PROTOCOL_VERSION,
                    revision,
                    token,
                },
            ))
        }
        ClientFrame::Request { .. } => Err("Authenticate before sending requests"),
    }
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
        RemoteRequest::GetSession { session_id } => sessions
            .get_session(&SessionId::from(session_id))
            .await
            .map(|session| RemoteResponse::Session {
                session: session.map(RemoteSession::from),
            }),
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
