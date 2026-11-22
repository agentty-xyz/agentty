//! One stream: a device's connection joined to the one its instance dials.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use tokio::sync::oneshot;
use tokio::time::{interval, timeout};

use crate::close::CloseReason;
use crate::limit::ByteBudget;
use crate::peer::Peer;
use crate::server::RelayState;

/// Random bytes in a stream identifier.
const STREAM_ID_BYTES: usize = 16;

/// Opens a stream for `device` toward `instance_id`, waits for the instance
/// to answer, and forwards messages until either side leaves.
///
/// Messages the device sends before the answer stay in its socket and are
/// forwarded once the stream is joined.
pub(crate) async fn serve_device(device: WebSocket, state: Arc<RelayState>, instance_id: String) {
    let _open_socket = state.track_socket();
    let mut device = Peer::new(device, state.limits.write_timeout);
    let (_open_stream, answer) = match OpenStream::open(&state, instance_id) {
        Ok(opened) => opened,
        Err(reason) => {
            device.close(reason).await;

            return;
        }
    };
    let answered = tokio::select! {
        biased;
        () = state.stopping() => Err(CloseReason::RelayStopped),
        answered = timeout(state.limits.accept_timeout, answer) => {
            answered.ok().and_then(Result::ok).ok_or(CloseReason::NoAnswer)
        }
    };

    match answered {
        Ok(instance) => {
            let instance = Peer::new(instance, state.limits.write_timeout);
            splice(device, instance, &state).await;
        }
        Err(reason) => device.close(reason).await,
    }
}

/// Hands `instance` to the device waiting on `stream_id` and returns whether
/// the device took it. The socket is dropped when the stream was answered or
/// closed in the meantime.
pub(crate) fn answer(instance: WebSocket, state: &RelayState, stream_id: &str) -> bool {
    state
        .registry
        .accept_stream(stream_id)
        .is_some_and(|answer_tx| answer_tx.send(instance).is_ok())
}

/// A stream counted against its instance until dropped.
struct OpenStream {
    instance_id: String,
    state: Arc<RelayState>,
    stream_id: String,
}

impl OpenStream {
    /// Names a new stream, announces it to the instance, and returns the
    /// receiver of the instance's answering socket.
    fn open(
        state: &Arc<RelayState>,
        instance_id: String,
    ) -> Result<(Self, oneshot::Receiver<WebSocket>), CloseReason> {
        let mut bytes = [0; STREAM_ID_BYTES];
        getrandom::fill(&mut bytes)?;
        let stream_id = hex::encode(bytes);
        let answer = state
            .registry
            .open_stream(&instance_id, &stream_id, &state.limits)?;
        let open_stream = Self {
            instance_id,
            state: Arc::clone(state),
            stream_id,
        };

        Ok((open_stream, answer))
    }
}

impl Drop for OpenStream {
    fn drop(&mut self) {
        self.state
            .registry
            .close_stream(&self.instance_id, &self.stream_id);
    }
}

/// Forwards binary messages between the two peers until one side leaves,
/// stops reading, breaks a rule, or the relay stops, then closes both with
/// the same reason.
async fn splice(mut device: Peer, mut instance: Peer, state: &RelayState) {
    let mut budget = ByteBudget::new(state.limits.max_bytes_per_minute);
    let mut ping = interval(state.limits.ping_interval);
    let reason = loop {
        let ended = tokio::select! {
            () = state.stopping() => Some(CloseReason::RelayStopped),
            _ = ping.tick() => {
                device.send(Message::Ping(Vec::new().into())).await;
                instance.send(Message::Ping(Vec::new().into())).await;

                None
            }
            message = device.receive() => forward(message, &mut instance, &mut budget).await,
            message = instance.receive() => forward(message, &mut device, &mut budget).await,
        };
        if let Some(reason) = ended {
            break reason;
        }
    };
    device.close(reason).await;
    instance.close(reason).await;
}

/// Forwards one received message to the other side. Returns the reason to
/// end the stream, if any. A peer that does not take the message in time
/// counts as gone.
async fn forward(
    message: Option<Result<Message, axum::Error>>,
    to: &mut Peer,
    budget: &mut ByteBudget,
) -> Option<CloseReason> {
    match message {
        Some(Ok(Message::Binary(bytes))) => {
            if !budget.spend(bytes.len()) {
                return Some(CloseReason::RateLimited);
            }
            if !to.send(Message::Binary(bytes)).await {
                return Some(CloseReason::PeerLeft);
            }

            None
        }
        Some(Ok(Message::Ping(_) | Message::Pong(_))) => None,
        Some(Ok(Message::Text(_))) => Some(CloseReason::TextMessage),
        Some(Ok(Message::Close(_)) | Err(_)) | None => Some(CloseReason::PeerLeft),
    }
}
