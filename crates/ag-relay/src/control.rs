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
/// the instance has proven its key.
pub(crate) async fn serve(
    socket: WebSocket,
    state: Arc<RelayState>,
    address: IpAddr,
    handshake: HandshakeGuard,
) {
    let _open_socket = state.track_socket();
    let mut instance = Peer::new(socket, state.limits.write_timeout);
    let authenticated = tokio::select! {
        biased;
        () = state.stopping() => Err(CloseReason::RelayStopped),
        key = timeout(state.limits.handshake_timeout, authenticate(&mut instance)) => {
            key.ok().flatten().ok_or(CloseReason::HandshakeFailed)
        }
    };
    drop(handshake);
    let registered = authenticated.and_then(|key| {
        let instance_id = key.fingerprint();
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
