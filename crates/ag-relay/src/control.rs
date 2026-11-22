//! Control connection of one instance: registration and stream notices.

use std::net::IpAddr;
use std::sync::Arc;

use ag_noise::{Handshake, PublicKey};
use axum::extract::ws::{Message, WebSocket};
use tokio::sync::{OwnedSemaphorePermit, mpsc};
use tokio::time::{interval, timeout};

use crate::close::CloseReason;
use crate::server::{REGISTERED_MESSAGE, RelayState, STREAM_MESSAGE_PREFIX};

/// Runs one control connection until the instance leaves, a newer
/// registration replaces it, or the relay stops. `handshake` is held until
/// the instance has proven its key.
pub(crate) async fn serve(
    mut socket: WebSocket,
    state: Arc<RelayState>,
    address: IpAddr,
    handshake: OwnedSemaphorePermit,
) {
    let _open_socket = state.track_socket();
    let authenticated = tokio::select! {
        biased;
        () = state.stopping() => Err(CloseReason::RelayStopped),
        instance = timeout(state.limits.handshake_timeout, authenticate(&mut socket)) => {
            instance.ok().flatten().ok_or(CloseReason::HandshakeFailed)
        }
    };
    drop(handshake);
    let registered = authenticated.and_then(|instance| {
        let instance_id = instance.fingerprint();
        let registration = state
            .registry
            .register(&instance_id, address, &state.limits)?;

        Ok((instance_id, registration))
    });
    let (instance_id, mut registration) = match registered {
        Ok(registered) => registered,
        Err(reason) => {
            reason.close(&mut socket).await;

            return;
        }
    };
    let _ = socket.send(Message::Text(REGISTERED_MESSAGE.into())).await;
    let reason = announce_streams(&mut socket, &state, &mut registration.streams).await;
    state
        .registry
        .unregister(&instance_id, registration.generation);
    if let Some(reason) = reason {
        reason.close(&mut socket).await;
    }
}

/// Runs the relay side of the registration handshake and returns the key
/// the instance proved, or `None` when the handshake fails.
async fn authenticate(socket: &mut WebSocket) -> Option<PublicKey> {
    let mut handshake = Handshake::relay_responder().ok()?;
    let first = receive_binary(socket).await?;
    handshake.read_message(&first).ok()?;
    let second = handshake.write_message().ok()?;
    socket.send(Message::Binary(second.into())).await.ok()?;
    let third = receive_binary(socket).await?;
    handshake.read_message(&third).ok()?;

    handshake.remote_public_key()
}

/// Receives the next message when it is binary.
async fn receive_binary(socket: &mut WebSocket) -> Option<Vec<u8>> {
    match socket.recv().await {
        Some(Ok(Message::Binary(bytes))) => Some(bytes.to_vec()),
        _ => None,
    }
}

/// Sends the instance each stream identifier and keepalive pings until the
/// connection ends. Returns the reason to report when the relay ends it,
/// and `None` when the instance left.
async fn announce_streams(
    socket: &mut WebSocket,
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
                let _ = socket.send(Message::Text(notice.into())).await;
            }
            _ = ping.tick() => {
                let _ = socket.send(Message::Ping(Vec::new().into())).await;
            }
            message = socket.recv() => {
                if matches!(message, Some(Ok(Message::Close(_)) | Err(_)) | None) {
                    return None;
                }
            }
        }
    }
}
