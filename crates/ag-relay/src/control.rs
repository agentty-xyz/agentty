//! Control connection of one instance: registration and stream notices.

use std::net::IpAddr;
use std::sync::Arc;

use ag_noise::{Handshake, PublicKey};
use axum::extract::ws::{Message, WebSocket};
use tokio::sync::mpsc;
use tokio::time::{interval, timeout};

use crate::close::CloseReason;
use crate::limit::HandshakeGuard;
use crate::peer::Peer;
use crate::server::{REGISTERED_MESSAGE, RelayState, STREAM_MESSAGE_PREFIX};

/// Runs one control connection until the instance leaves, a newer
/// registration replaces it, or the relay stops. `handshake` is held until
/// the instance has proven its key and been admitted, so admission
/// decisions in progress count against the handshake limits.
pub(crate) async fn serve(
    socket: WebSocket,
    state: Arc<RelayState>,
    address: IpAddr,
    handshake: HandshakeGuard,
) {
    let _open_socket = state.track_socket();
    let mut instance = Peer::new(socket, state.limits.write_timeout);
    let admitted = tokio::select! {
        biased;
        () = state.stopping() => Err(CloseReason::RelayStopped),
        admitted = authenticate_and_admit(&mut instance, &state) => admitted,
    };
    drop(handshake);
    let registered = admitted.and_then(|instance_id| {
        let registration = state
            .registry
            .register(&instance_id, address, &state.limits)?;

        Ok((instance_id, registration))
    });
    let (instance_id, mut registration) = match registered {
        Ok(registered) => registered,
        Err(reason) => {
            instance.close(reason).await;

            return;
        }
    };
    instance
        .send(Message::Text(REGISTERED_MESSAGE.into()))
        .await;
    let reason = announce_streams(&mut instance, &state, &mut registration.streams).await;
    state
        .registry
        .unregister(&instance_id, registration.generation);
    if let Some(reason) = reason {
        instance.close(reason).await;
    }
}

/// Proves the instance's key within [`crate::RelayLimits::handshake_timeout`]
/// and asks the admission policy about its identifier within
/// [`crate::RelayLimits::admission_timeout`]. Returns the admitted identifier.
///
/// A policy error or timeout refuses the instance as an unavailable relay,
/// since a later attempt may succeed; an explicit refusal is reported as
/// such.
async fn authenticate_and_admit(
    instance: &mut Peer,
    state: &RelayState,
) -> Result<String, CloseReason> {
    let key = timeout(state.limits.handshake_timeout, authenticate(instance))
        .await
        .ok()
        .flatten()
        .ok_or(CloseReason::HandshakeFailed)?;
    let instance_id = key.fingerprint();
    let decision = timeout(
        state.limits.admission_timeout,
        state.admission.admit(&instance_id),
    )
    .await;

    match decision {
        Ok(Ok(true)) => Ok(instance_id),
        Ok(Ok(false)) => Err(CloseReason::NotAdmitted),
        Ok(Err(_)) | Err(_) => Err(CloseReason::Unavailable),
    }
}

/// Runs the relay side of the registration handshake and returns the key
/// the instance proved, or `None` when the handshake fails.
async fn authenticate(instance: &mut Peer) -> Option<PublicKey> {
    let mut handshake = Handshake::relay_responder().ok()?;
    let first = receive_binary(instance).await?;
    handshake.read_message(&first).ok()?;
    let second = handshake.write_message().ok()?;
    instance.send(Message::Binary(second.into())).await;
    let third = receive_binary(instance).await?;
    handshake.read_message(&third).ok()?;

    handshake.remote_public_key()
}

/// Receives the next message when it is binary.
async fn receive_binary(instance: &mut Peer) -> Option<Vec<u8>> {
    match instance.receive().await {
        Some(Ok(Message::Binary(bytes))) => Some(bytes.to_vec()),
        _ => None,
    }
}

/// Sends the instance each stream identifier and keepalive pings until the
/// connection ends. Returns the reason to report when the relay ends it,
/// and `None` when the instance left.
async fn announce_streams(
    instance: &mut Peer,
    state: &RelayState,
    streams: &mut mpsc::Receiver<String>,
) -> Option<CloseReason> {
    let mut ping = interval(state.limits.ping_interval);

    loop {
        tokio::select! {
            biased;
            () = state.stopping() => return Some(CloseReason::RelayStopped),
            stream_id = streams.recv() => {
                let Some(stream_id) = stream_id else {
                    return Some(CloseReason::Replaced);
                };
                let notice = format!("{STREAM_MESSAGE_PREFIX}{stream_id}");
                instance.send(Message::Text(notice.into())).await;
            }
            _ = ping.tick() => {
                instance.send(Message::Ping(Vec::new().into())).await;
            }
            message = instance.receive() => {
                if matches!(message, Some(Ok(Message::Close(_)) | Err(_)) | None) {
                    return None;
                }
            }
        }
    }
}
