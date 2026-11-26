//! Frame transport: the duplex stream a connection reads client frames from
//! and writes server frames to.

/// Duplex stream of text frames between one device and the server.
///
/// Connections depend on this trait instead of the encrypted relay stream,
/// so their protocol handling stays apart from how frames are carried.
pub(crate) trait FrameTransport {
    /// Waits for the next thing the device sends.
    ///
    /// Must be cancel-safe: connections race it against timers and server
    /// events, and a dropped call must not lose a message.
    async fn receive(&mut self) -> Incoming;

    /// Sends one text frame. A failed send surfaces as the next receive
    /// reporting [`Incoming::Closed`].
    async fn send(&mut self, text: String);

    /// Ends the connection. A device the server rejects learns why from the
    /// frame sent before this.
    async fn close(&mut self);
}

/// One thing received from the device side of a transport.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Incoming {
    /// A frame that is not text, which the protocol does not use.
    Binary,
    /// The device left or the transport failed.
    Closed,
    /// One text frame, expected to hold a client frame.
    Text(String),
}
