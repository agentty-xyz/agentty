//! Reasons the relay ends a connection.

use axum::extract::ws::{CloseFrame, Message, WebSocket, close_code};

/// Why the relay closes a connection, reported in the close frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CloseReason {
    /// The registration handshake failed or timed out.
    HandshakeFailed,
    /// The instance has too many unanswered streams.
    InstanceBusy,
    /// The instance did not answer the stream in time.
    NoAnswer,
    /// The other end of the stream left.
    PeerLeft,
    /// The stream carried more bytes than its budget allows.
    RateLimited,
    /// The relay holds as many instances as it allows.
    RelayFull,
    /// The relay is stopping.
    RelayStopped,
    /// A newer registration of the same instance took over.
    Replaced,
    /// A stream carried a text message.
    TextMessage,
    /// The client address registered as many instances as it may.
    TooManyInstances,
    /// The instance holds as many streams as it may.
    TooManyStreams,
    /// The relay could not create a stream identifier.
    Unavailable,
    /// No instance is registered under the requested identifier.
    UnknownInstance,
}

impl CloseReason {
    /// Text sent as the close reason.
    pub(crate) fn text(self) -> &'static str {
        match self {
            Self::HandshakeFailed => "Handshake failed",
            Self::InstanceBusy => "Instance is busy",
            Self::NoAnswer => "Instance did not answer",
            Self::PeerLeft => "Peer left",
            Self::RateLimited => "Rate limit exceeded",
            Self::RelayFull => "Relay is full",
            Self::RelayStopped => "Relay stopped",
            Self::Replaced => "Replaced by a newer registration",
            Self::TextMessage => "Text messages are not supported",
            Self::TooManyInstances => "Too many instances for this address",
            Self::TooManyStreams => "Too many streams for this instance",
            Self::Unavailable => "Relay is unavailable",
            Self::UnknownInstance => "Unknown instance",
        }
    }

    /// WebSocket close code: normal for a peer leaving, going-away for a
    /// stopping relay or a missing answer, and policy violation otherwise.
    pub(crate) fn code(self) -> u16 {
        match self {
            Self::PeerLeft => close_code::NORMAL,
            Self::NoAnswer | Self::RelayStopped | Self::Unavailable => close_code::AWAY,
            Self::HandshakeFailed
            | Self::InstanceBusy
            | Self::RateLimited
            | Self::RelayFull
            | Self::Replaced
            | Self::TextMessage
            | Self::TooManyInstances
            | Self::TooManyStreams
            | Self::UnknownInstance => close_code::POLICY,
        }
    }

    /// Starts the closing handshake on `socket` with this reason.
    pub(crate) async fn close(self, socket: &mut WebSocket) {
        let _ = socket
            .send(Message::Close(Some(CloseFrame {
                code: self.code(),
                reason: self.text().into(),
            })))
            .await;
    }
}

impl From<getrandom::Error> for CloseReason {
    /// A failed random source leaves the relay unable to name streams.
    fn from(_: getrandom::Error) -> Self {
        Self::Unavailable
    }
}

#[cfg(test)]
#[path = "close_test.rs"]
mod tests;
