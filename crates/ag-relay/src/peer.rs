//! One connected client with bounded sends.

use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use tokio::time::timeout;

use crate::close::CloseReason;

/// A client's WebSocket whose sends give up when the client stops reading,
/// so a stalled client cannot hold its connection task forever.
pub(crate) struct Peer {
    socket: WebSocket,
    write_timeout: Duration,
}

impl Peer {
    /// Wraps `socket`; each send waits at most `write_timeout`.
    pub(crate) fn new(socket: WebSocket, write_timeout: Duration) -> Self {
        Self {
            socket,
            write_timeout,
        }
    }

    /// Waits for the next message. Cancel-safe.
    pub(crate) async fn receive(&mut self) -> Option<Result<Message, axum::Error>> {
        self.socket.recv().await
    }

    /// Sends one message and returns whether the client took it in time.
    pub(crate) async fn send(&mut self, message: Message) -> bool {
        let sent = timeout(self.write_timeout, self.socket.send(message)).await;

        matches!(sent, Ok(Ok(())))
    }

    /// Starts the closing handshake with `reason`.
    pub(crate) async fn close(&mut self, reason: CloseReason) {
        let frame = CloseFrame {
            code: reason.code(),
            reason: reason.text().into(),
        };
        self.send(Message::Close(Some(frame))).await;
    }
}
