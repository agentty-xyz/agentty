//! One client connection over a frame transport: authentication, pairing
//! approval, requests, and change pushes.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use ag_noise::PublicKey;
use ag_session::{
    AnswerQuestionsRequest, CreateSessionRequest, GenerateGraphProposalRequest, QuestionAnswer,
    SessionError, SessionGraphsRequest, SessionId, SessionOperation, SessionService,
    SessionSettingsChange,
};
use tokio::sync::oneshot::error::TryRecvError;
use tokio::sync::{oneshot, watch};
use tokio::task::{self, JoinSet};
use tokio::time::{Instant, sleep_until, timeout};

use crate::PROTOCOL_VERSION;
use crate::device::{NewRemoteDevice, RemoteDevice, key_hash, token_hash};
use crate::entropy::{Entropy, random_hex};
use crate::pairing::random_code;
use crate::protocol::{
    ClientFrame, RemoteGraphActionRequest, RemoteProject, RemoteQuestionAnswer, RemoteRequest,
    RemoteResponse, RemoteResult, RemoteReviewRequest, RemoteSession, RemoteSessionMode,
    RemoteSessionSummary, ServerFrame,
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
/// Error returned for a request method this server does not know.
const UNSUPPORTED_REQUEST: &str = "Unsupported request";
/// Requests one connection may have running at once.
pub(crate) const MAX_REQUESTS_IN_PROGRESS: usize = 8;
/// Error returned for a request beyond [`MAX_REQUESTS_IN_PROGRESS`].
pub(crate) const TOO_MANY_REQUESTS: &str = "Too many requests in progress";
/// Error returned for a request whose task failed before it could reply.
const REQUEST_FAILED: &str = "Request failed";
/// Optional requests, each named by the capability that offers it and served
/// only when the host supports its operation.
const CAPABILITIES: &[(SessionOperation, &str)] = &[
    (SessionOperation::HostContext, "host"),
    (SessionOperation::CreateSession, "create_session"),
    (SessionOperation::MergeSession, "merge"),
    (SessionOperation::SessionDiff, "diff"),
    (SessionOperation::SessionFiles, "files"),
    (SessionOperation::ProjectFiles, "project_files"),
    (SessionOperation::StopTurn, "stop_turn"),
    (SessionOperation::RebaseSession, "sync"),
    (SessionOperation::SessionSettings, "settings"),
    (SessionOperation::PublishSession, "publish"),
    (SessionOperation::SyncProject, "sync_project"),
    (SessionOperation::Graphs, "graphs"),
    (SessionOperation::GraphActions, "graph_actions"),
    (SessionOperation::GenerateGraphProposal, "graph_proposal"),
];
/// Random bytes in the delivery identity of a graph proposal a device asks
/// for.
const PROPOSAL_ID_BYTES: usize = 16;
/// Error returned when secure randomness for a proposal identity fails.
const PROPOSAL_ID_UNAVAILABLE: &str = "Cannot generate a graph proposal identity";
/// Error returned when a device asks for a creation mode the host does not
/// serve now.
const UNAVAILABLE_SESSION_MODE: &str = "Session mode is not available";
/// Error returned when a device asks for the patch of a file the session's
/// diff does not change.
const FILE_NOT_IN_DIFF: &str = "File is not in the session diff";

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
            capabilities: capabilities(state.sessions()),
            device_id: device.id,
            protocol_version: PROTOCOL_VERSION,
            revision,
        },
    })
}

/// Returns the optional requests this server supports, sent in `Welcome`
/// and `Paired`.
///
/// Requests added after the first protocol version are offered only through
/// this list, so a client never sends one to a server that lacks it. Each
/// is listed only when the host serves its operation.
fn capabilities(sessions: &SessionService) -> Vec<String> {
    CAPABILITIES
        .iter()
        .filter(|(operation, _)| sessions.supports(*operation))
        .map(|(_, capability)| (*capability).to_string())
        .collect()
}

/// Refuses an optional request whose operation the host does not serve, so
/// a client that skips the capability list still reaches nothing more.
fn require_operation(
    sessions: &SessionService,
    operation: SessionOperation,
) -> Result<(), SessionError> {
    if sessions.supports(operation) {
        return Ok(());
    }

    Err(SessionError::Operation(UNSUPPORTED_REQUEST.to_string()))
}

/// Parses one optional setting sent as its persisted name, such as `high`.
fn parse_setting<Setting: FromStr<Err = String>>(
    value: Option<String>,
) -> Result<Option<Setting>, SessionError> {
    value
        .map(|value| value.parse().map_err(SessionError::Operation))
        .transpose()
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
            capabilities: capabilities(state.sessions()),
            device_id: device.device_id,
            protocol_version: PROTOCOL_VERSION,
            revision,
            token: device.token,
        },
    ))
}

/// Serves requests and change pushes for one authenticated connection.
///
/// Each request runs in its own task, so a slow host operation delays
/// neither later requests nor change pushes. Replies go out as they
/// complete, matched to their request by `id`. Once the connection ends,
/// the replies still pending are dropped, and the connection stays counted
/// as open until the host work behind them finishes.
async fn run<T: FrameTransport>(
    mut transport: T,
    state: &ServerState,
    mut revisions: watch::Receiver<u64>,
    mut shutdown_rx: watch::Receiver<bool>,
    mut close_rx: oneshot::Receiver<()>,
) {
    let mut requests = RequestTasks::default();
    serve_authenticated(
        &mut transport,
        state,
        &mut requests,
        &mut revisions,
        &mut shutdown_rx,
        &mut close_rx,
    )
    .await;
    requests.finish().await;
}

/// Serves requests and change pushes until the connection ends, starting
/// each request in `requests`.
async fn serve_authenticated<T: FrameTransport>(
    transport: &mut T,
    state: &ServerState,
    requests: &mut RequestTasks,
    revisions: &mut watch::Receiver<u64>,
    shutdown_rx: &mut watch::Receiver<bool>,
    close_rx: &mut oneshot::Receiver<()>,
) {
    let mut change_deadline: Option<Instant> = None;
    let mut revisions_open = true;

    loop {
        tokio::select! {
            biased;
            () = wait_until_stopping(shutdown_rx) => {
                transport.close(CloseCause::Stopping, SERVER_STOPPED).await;

                return;
            }
            _ = &mut *close_rx => {
                reject(transport, "Device revoked").await;

                return;
            }
            () = sleep_until(change_deadline.unwrap_or_else(Instant::now)),
                if change_deadline.is_some() =>
            {
                change_deadline = None;
                let revision = *revisions.borrow_and_update();
                send(transport, &ServerFrame::Changed { revision }).await;
            }
            changed = revisions.changed(), if revisions_open && change_deadline.is_none() => {
                if changed.is_ok() {
                    change_deadline = Some(Instant::now() + CHANGE_DEBOUNCE);
                } else {
                    revisions_open = false;
                }
            }
            Some(reply) = requests.next_reply(), if !requests.is_empty() => {
                send(transport, &reply).await;
            }
            incoming = transport.receive() => {
                match handle_incoming(incoming) {
                    MessageOutcome::Closed => return,
                    MessageOutcome::Ignored => {}
                    MessageOutcome::Rejected(reason) => {
                        reject(transport, reason).await;

                        return;
                    }
                    MessageOutcome::Request { id, request } => {
                        if !requests.start(state, id, request) {
                            let busy = ServerFrame::Response {
                                id,
                                result: RemoteResult::Error(TOO_MANY_REQUESTS.to_string()),
                            };
                            send(transport, &busy).await;
                        }
                    }
                }
            }
        }
    }
}

/// Requests one connection is running, each in its own task.
#[derive(Default)]
struct RequestTasks {
    /// Request `id` of each task, for replying when a task panics.
    request_ids: HashMap<task::Id, u64>,
    /// For each session with a mutation running, a receiver that closes
    /// once the latest of those mutations finishes.
    session_queues: HashMap<String, oneshot::Receiver<()>>,
    /// Running tasks and finished ones whose reply is not yet sent.
    tasks: JoinSet<ServerFrame>,
}

impl RequestTasks {
    /// Returns whether no task is running or waiting to reply.
    fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Starts `request` and returns `true`, or returns `false` when
    /// [`MAX_REQUESTS_IN_PROGRESS`] requests are already in progress.
    ///
    /// A request that changes a session waits for the earlier ones that
    /// change the same session, so they reach the host in arrival order.
    fn start(&mut self, state: &ServerState, id: u64, request: RemoteRequest) -> bool {
        if self.tasks.len() >= MAX_REQUESTS_IN_PROGRESS {
            return false;
        }
        let queue = mutated_session(&request).map(|session_id| self.enqueue(session_id));
        let sessions = state.sessions().clone();
        let entropy = Arc::clone(&state.entropy);
        let handle = self.tasks.spawn(async move {
            let _done = match queue {
                Some((previous, done)) => {
                    if let Some(previous) = previous {
                        // Closes, without a value, when the earlier
                        // mutation finishes.
                        let _ = previous.await;
                    }

                    Some(done)
                }
                None => None,
            };

            ServerFrame::Response {
                id,
                result: execute(&sessions, entropy.as_ref(), request).await,
            }
        });
        self.request_ids.insert(handle.id(), id);

        true
    }

    /// Queues one more mutation of `session_id`, returning the receiver of
    /// the mutation it must wait for, if any, and the sender whose drop
    /// releases the next one.
    fn enqueue(
        &mut self,
        session_id: &str,
    ) -> (Option<oneshot::Receiver<()>>, oneshot::Sender<()>) {
        // Forget sessions whose last mutation already finished, so the map
        // never outgrows the requests in progress.
        self.session_queues
            .retain(|_, previous| matches!(previous.try_recv(), Err(TryRecvError::Empty)));
        let (done, next) = oneshot::channel();
        let previous = self.session_queues.insert(session_id.to_string(), next);

        (previous, done)
    }

    /// Waits for the next finished request and returns its reply. A
    /// request whose task panicked gets an error reply.
    ///
    /// Cancel-safe: a dropped call loses no reply.
    async fn next_reply(&mut self) -> Option<ServerFrame> {
        let (task_id, reply) = match self.tasks.join_next_with_id().await? {
            Ok((task_id, frame)) => (task_id, frame),
            Err(error) => {
                let id = self.request_ids.get(&error.id()).copied()?;
                tracing::warn!(id, "remote request task failed");
                let frame = ServerFrame::Response {
                    id,
                    result: RemoteResult::Error(REQUEST_FAILED.to_string()),
                };

                (error.id(), frame)
            }
        };
        self.request_ids.remove(&task_id);

        Some(reply)
    }

    /// Waits for every request still in progress to finish, without
    /// sending its reply.
    async fn finish(mut self) {
        while self.tasks.join_next().await.is_some() {}
    }
}

/// Returns the session a request changes, or `None` for reads, for creation
/// and a project sync, which change no existing session, and for a publish.
/// A publish can wait
/// behind a running turn, so queueing it would hold a later stop, cancel,
/// answer, or message until the turn ends; the host serializes the push
/// itself.
fn mutated_session(request: &RemoteRequest) -> Option<&str> {
    match request {
        RemoteRequest::ActOnSessionGraph(request) => Some(&request.session_id),
        RemoteRequest::AnswerQuestions { session_id, .. }
        | RemoteRequest::CancelSession { session_id }
        | RemoteRequest::GenerateGraphProposal { session_id, .. }
        | RemoteRequest::MergeSession { session_id }
        | RemoteRequest::SendMessage { session_id, .. }
        | RemoteRequest::StopTurn { session_id }
        | RemoteRequest::SyncSession { session_id }
        | RemoteRequest::SetSessionSettings { session_id, .. } => Some(session_id),
        RemoteRequest::CreateSession { .. }
        | RemoteRequest::GetHost
        | RemoteRequest::GetSession { .. }
        | RemoteRequest::GetSessionDiff { .. }
        | RemoteRequest::GetSessionGraphs { .. }
        | RemoteRequest::ListProjectFiles
        | RemoteRequest::ListSessionFiles { .. }
        | RemoteRequest::ListSessions
        | RemoteRequest::PublishSession { .. }
        | RemoteRequest::SyncProject
        | RemoteRequest::Unsupported => None,
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
    /// Run this request and reply to it under `id`.
    Request {
        /// Identifier the reply carries.
        id: u64,
        /// Request to run.
        request: RemoteRequest,
    },
}

/// Classifies one thing received on an authenticated connection.
pub(crate) fn handle_incoming(incoming: Incoming) -> MessageOutcome {
    match incoming {
        Incoming::Text(text) => match serde_json::from_str::<ClientFrame>(&text) {
            Ok(ClientFrame::Request { id, request }) => MessageOutcome::Request { id, request },
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

/// Runs one request against the session service, drawing the identities it
/// mints from `entropy`.
pub(crate) async fn execute(
    sessions: &SessionService,
    entropy: &dyn Entropy,
    request: RemoteRequest,
) -> RemoteResult {
    match respond(sessions, entropy, request).await {
        Ok(response) => RemoteResult::Ok(response),
        Err(error) => RemoteResult::Error(error.to_string()),
    }
}

/// Answers one request.
///
/// Every request that names a session first checks that the host lists it,
/// so a remote client reaches only the sessions it can see.
async fn respond(
    sessions: &SessionService,
    entropy: &dyn Entropy,
    request: RemoteRequest,
) -> Result<RemoteResponse, SessionError> {
    match request {
        RemoteRequest::ActOnSessionGraph(request) => {
            act_on_listed_session_graph(sessions, request).await
        }
        RemoteRequest::AnswerQuestions {
            answers,
            graph_question_target,
            session_id,
        } => answer_listed_questions(sessions, session_id, answers, graph_question_target).await,
        RemoteRequest::CancelSession { session_id } => {
            let session_id = listed_session_id(sessions, session_id).await?;
            sessions.cancel_session(&session_id).await?;

            Ok(RemoteResponse::Accepted)
        }
        RemoteRequest::CreateSession { mode } => create_session(sessions, mode).await,
        RemoteRequest::GenerateGraphProposal {
            message,
            session_id,
        } => generate_listed_graph_proposal(sessions, entropy, session_id, message).await,
        RemoteRequest::GetHost => {
            require_operation(sessions, SessionOperation::HostContext)?;

            Ok(RemoteResponse::from(sessions.host_context().await?))
        }
        RemoteRequest::GetSession { session_id } => get_listed_session(sessions, session_id).await,
        RemoteRequest::GetSessionDiff { path, session_id } => {
            get_listed_session_diff(sessions, session_id, path.as_deref()).await
        }
        RemoteRequest::GetSessionGraphs {
            graph_id,
            known,
            node_id,
            session_id,
        } => {
            get_listed_session_graphs(sessions, session_id, graph_id, node_id, known.as_deref())
                .await
        }
        RemoteRequest::ListProjectFiles => list_project_files(sessions).await,
        RemoteRequest::ListSessionFiles { session_id } => {
            list_listed_session_files(sessions, session_id).await
        }
        RemoteRequest::ListSessions => list_sessions(sessions).await,
        RemoteRequest::MergeSession { session_id } => {
            require_operation(sessions, SessionOperation::MergeSession)?;
            let session_id = listed_session_id(sessions, session_id).await?;
            sessions.merge_session(&session_id).await?;

            Ok(RemoteResponse::Accepted)
        }
        RemoteRequest::PublishSession { session_id } => {
            publish_listed_session(sessions, session_id).await
        }
        RemoteRequest::SendMessage {
            message,
            session_id,
        } => {
            let session_id = listed_session_id(sessions, session_id).await?;
            sessions.send_message(&session_id, message).await?;

            Ok(RemoteResponse::Accepted)
        }
        RemoteRequest::StopTurn { session_id } => {
            require_operation(sessions, SessionOperation::StopTurn)?;
            let session_id = listed_session_id(sessions, session_id).await?;
            sessions.stop_turn(&session_id).await?;

            Ok(RemoteResponse::Accepted)
        }
        RemoteRequest::SyncProject => {
            require_operation(sessions, SessionOperation::SyncProject)?;
            sessions.sync_project().await?;

            Ok(RemoteResponse::Accepted)
        }
        RemoteRequest::SyncSession { session_id } => {
            require_operation(sessions, SessionOperation::RebaseSession)?;
            let session_id = listed_session_id(sessions, session_id).await?;
            sessions.rebase_session(&session_id).await?;

            Ok(RemoteResponse::Accepted)
        }
        RemoteRequest::SetSessionSettings {
            model,
            reasoning,
            response_style,
            session_id,
            speed_mode,
        } => {
            require_operation(sessions, SessionOperation::SessionSettings)?;
            let change = SessionSettingsChange {
                model: parse_setting(model)?,
                reasoning_level: parse_setting(reasoning)?,
                response_style: parse_setting(response_style)?,
                speed_mode: parse_setting(speed_mode)?,
            };
            let session_id = listed_session_id(sessions, session_id).await?;
            sessions
                .update_session_settings(&session_id, change)
                .await?;

            Ok(RemoteResponse::Accepted)
        }
        RemoteRequest::Unsupported => Err(SessionError::Operation(UNSUPPORTED_REQUEST.to_string())),
    }
}

/// Applies one graph inspector action only when the host lists the session
/// that owns the graph; the host checks the graph's ownership, snapshot, and
/// the action's availability.
async fn act_on_listed_session_graph(
    sessions: &SessionService,
    request: RemoteGraphActionRequest,
) -> Result<RemoteResponse, SessionError> {
    require_operation(sessions, SessionOperation::GraphActions)?;
    let session_id = listed_session_id(sessions, request.session_id.clone()).await?;
    sessions
        .act_on_session_graph(request.into_session_request(session_id)?)
        .await?;

    Ok(RemoteResponse::Accepted)
}

/// Publishes one session and opens or refreshes its review request only
/// when the host lists that session.
async fn publish_listed_session(
    sessions: &SessionService,
    session_id: String,
) -> Result<RemoteResponse, SessionError> {
    require_operation(sessions, SessionOperation::PublishSession)?;
    let session_id = listed_session_id(sessions, session_id).await?;
    let review_request = sessions.create_review_request(&session_id).await?;

    Ok(RemoteResponse::ReviewRequest {
        review_request: RemoteReviewRequest::from(review_request.summary),
    })
}

/// Asks for a graph proposal for the task `message` only when the host
/// lists the session, under a fresh delivery identity; the host checks the
/// session's readiness and the task's size.
async fn generate_listed_graph_proposal(
    sessions: &SessionService,
    entropy: &dyn Entropy,
    session_id: String,
    message: String,
) -> Result<RemoteResponse, SessionError> {
    require_operation(sessions, SessionOperation::GenerateGraphProposal)?;
    let session_id = listed_session_id(sessions, session_id).await?;
    let id = random_hex(entropy, PROPOSAL_ID_BYTES)
        .ok_or_else(|| SessionError::Operation(PROPOSAL_ID_UNAVAILABLE.to_string()))?;
    sessions
        .generate_graph_proposal(GenerateGraphProposalRequest::from_text(
            format!("remote-{id}"),
            session_id,
            message,
        ))
        .await?;

    Ok(RemoteResponse::Accepted)
}

/// Answers the current question set of one session only when the host lists
/// that session: its turn's, or with `graph_question_target` the graph
/// questions of the graph work that key names.
async fn answer_listed_questions(
    sessions: &SessionService,
    session_id: String,
    answers: Vec<RemoteQuestionAnswer>,
    graph_question_target: Option<String>,
) -> Result<RemoteResponse, SessionError> {
    let session_id = listed_session_id(sessions, session_id).await?;
    let request = AnswerQuestionsRequest {
        answers: answers
            .into_iter()
            .map(|answer| QuestionAnswer {
                answer: answer.answer,
                question: answer.question,
            })
            .collect(),
        attachments: Vec::new(),
        graph_question_target,
    };
    sessions.answer_questions(&session_id, request).await?;

    Ok(RemoteResponse::Accepted)
}

/// Creates a session in the host's active project, the only project the
/// host accepts, and only in a mode the host serves now.
async fn create_session(
    sessions: &SessionService,
    mode: RemoteSessionMode,
) -> Result<RemoteResponse, SessionError> {
    require_operation(sessions, SessionOperation::CreateSession)?;
    let host = sessions.host_context().await?;
    let mode = mode.session_mode();
    if !host
        .session_modes
        .iter()
        .any(|session_mode| session_mode.mode == mode)
    {
        return Err(SessionError::Operation(
            UNAVAILABLE_SESSION_MODE.to_string(),
        ));
    }
    let session_id = sessions
        .create_session(CreateSessionRequest {
            inherit_from_session_id: None,
            mode,
            project_id: host.project_id,
        })
        .await?;

    Ok(RemoteResponse::Created {
        session_id: session_id.to_string(),
    })
}

/// Lists the host's sessions and, when the host reports it, the active
/// project they belong to.
async fn list_sessions(sessions: &SessionService) -> Result<RemoteResponse, SessionError> {
    let summaries = sessions.list_session_entries().await?;
    let project = if sessions.supports(SessionOperation::ProjectStatus) {
        Some(RemoteProject::from(sessions.project_status().await?))
    } else {
        None
    };

    Ok(RemoteResponse::Sessions {
        project,
        sessions: summaries
            .into_iter()
            .map(RemoteSessionSummary::from)
            .collect(),
    })
}

/// Loads one session only when the host lists it, so remote clients read
/// nothing beyond the sessions they can see.
async fn get_listed_session(
    sessions: &SessionService,
    session_id: String,
) -> Result<RemoteResponse, SessionError> {
    let session_id = match listed_session_id(sessions, session_id).await {
        Ok(session_id) => session_id,
        Err(SessionError::NotFound) => return Ok(RemoteResponse::Session { session: None }),
        Err(error) => return Err(error),
    };
    let session = sessions.get_session(&session_id).await?;

    Ok(RemoteResponse::Session {
        session: session.map(|session| Box::new(RemoteSession::from(session))),
    })
}

/// Reads the diff of one session only when the host lists it: the file
/// list, or with `path` that file's patch.
async fn get_listed_session_diff(
    sessions: &SessionService,
    session_id: String,
    path: Option<&str>,
) -> Result<RemoteResponse, SessionError> {
    require_operation(sessions, SessionOperation::SessionDiff)?;
    let session_id = listed_session_id(sessions, session_id).await?;
    let files = sessions
        .get_session_diff(&session_id)
        .await?
        .ok_or(SessionError::NotFound)?;

    RemoteResponse::from_diff(&files, path)
        .ok_or_else(|| SessionError::Operation(FILE_NOT_IN_DIFF.to_string()))
}

/// Reads the graphs of one session only when the host lists it, selecting
/// `graph_id` and `node_id` when given, and answers `unchanged` when the
/// device already shows that response as version `known`.
async fn get_listed_session_graphs(
    sessions: &SessionService,
    session_id: String,
    graph_id: Option<String>,
    node_id: Option<String>,
    known: Option<&str>,
) -> Result<RemoteResponse, SessionError> {
    require_operation(sessions, SessionOperation::Graphs)?;
    let session_id = listed_session_id(sessions, session_id).await?;
    let graphs = sessions
        .session_graphs(SessionGraphsRequest {
            graph_id,
            node_id,
            session_id,
        })
        .await?;

    Ok(RemoteResponse::from_graphs(graphs).unchanged_since(known))
}

/// Lists the files and directories in the host's active project checkout.
async fn list_project_files(sessions: &SessionService) -> Result<RemoteResponse, SessionError> {
    require_operation(sessions, SessionOperation::ProjectFiles)?;

    Ok(RemoteResponse::from_session_files(
        &sessions.list_project_files().await?,
    ))
}

/// Lists the files and directories in one session's worktree only when the
/// host lists that session.
async fn list_listed_session_files(
    sessions: &SessionService,
    session_id: String,
) -> Result<RemoteResponse, SessionError> {
    require_operation(sessions, SessionOperation::SessionFiles)?;
    let session_id = listed_session_id(sessions, session_id).await?;
    let entries = sessions
        .list_session_files(&session_id)
        .await?
        .ok_or(SessionError::NotFound)?;

    Ok(RemoteResponse::from_session_files(&entries))
}

/// Returns `session_id` when the host lists that session, and
/// [`SessionError::NotFound`] otherwise.
///
/// Remote clients see only the listed sessions, so every request that names
/// a session goes through this check before it reaches the host.
async fn listed_session_id(
    sessions: &SessionService,
    session_id: String,
) -> Result<SessionId, SessionError> {
    let is_listed = sessions
        .list_sessions()
        .await?
        .iter()
        .any(|summary| summary.id.as_str() == session_id);
    if !is_listed {
        return Err(SessionError::NotFound);
    }

    Ok(SessionId::from(session_id))
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
