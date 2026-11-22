//! Frame transport: the duplex stream a connection reads client frames from
//! and writes server frames to.

use axum::extract::ws::{CloseFrame, Message, WebSocket, close_code};

/// Duplex stream of text frames between one client and the server.
///
/// Connections depend on this trait instead of a concrete socket, so any
/// carrier that moves text frames can serve the same protocol.
pub(crate) trait FrameTransport {
    /// Waits for the next thing the client sends.
    ///
    /// Must be cancel-safe: connections race it against timers and server
    /// events, and a dropped call must not lose a message.
    async fn receive(&mut self) -> Incoming;

    /// Sends one text frame. A failed send surfaces as the next receive
    /// reporting [`Incoming::Closed`].
    async fn send(&mut self, text: String);

    /// Tells the client why the server ends the connection.
    async fn close(&mut self, cause: CloseCause, reason: &'static str);
}

/// One thing received from the client side of a transport.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Incoming {
    /// A binary message, which the protocol does not use.
    Binary,
    /// The client left or the transport failed.
    Closed,
    /// Keepalive traffic that carries no frame.
    Control,
    /// One text message, expected to hold a client frame.
    Text(String),
}

impl Incoming {
    /// Maps one received WebSocket item; a finished or failed stream counts
    /// as closed.
    pub(crate) fn from_websocket(message: Option<Result<Message, axum::Error>>) -> Self {
        match message {
            Some(Ok(Message::Text(text))) => Self::Text(text.to_string()),
            Some(Ok(Message::Binary(_))) => Self::Binary,
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => Self::Control,
            Some(Ok(Message::Close(_)) | Err(_)) | None => Self::Closed,
        }
    }
}

/// Why the server ends a connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloseCause {
    /// The client broke the protocol or lost its access.
    Rejected,
    /// The server is stopping.
    Stopping,
}

impl CloseCause {
    /// WebSocket close code that reports this cause.
    pub(crate) fn websocket_code(self) -> u16 {
        match self {
            Self::Rejected => close_code::POLICY,
            Self::Stopping => close_code::AWAY,
        }
    }
}

impl FrameTransport for WebSocket {
    async fn receive(&mut self) -> Incoming {
        Incoming::from_websocket(self.recv().await)
    }

    async fn send(&mut self, text: String) {
        let _ = WebSocket::send(self, Message::Text(text.into())).await;
    }

    async fn close(&mut self, cause: CloseCause, reason: &'static str) {
        let _ = WebSocket::send(
            self,
            Message::Close(Some(CloseFrame {
                code: cause.websocket_code(),
                reason: reason.into(),
            })),
        )
        .await;
    }
}

#[cfg(test)]
#[path = "transport_test.rs"]
mod tests;
