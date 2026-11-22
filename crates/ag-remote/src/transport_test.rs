use axum::extract::ws::{Message, close_code};

use super::{CloseCause, Incoming};

#[test]
fn from_websocket_maps_each_message_kind() {
    // Arrange
    let failure = axum::Error::new(std::io::Error::other("connection reset"));

    // Act
    let incoming = [
        Incoming::from_websocket(Some(Ok(Message::Text("frame".into())))),
        Incoming::from_websocket(Some(Ok(Message::Binary(vec![1].into())))),
        Incoming::from_websocket(Some(Ok(Message::Ping(Vec::new().into())))),
        Incoming::from_websocket(Some(Ok(Message::Pong(Vec::new().into())))),
        Incoming::from_websocket(Some(Ok(Message::Close(None)))),
        Incoming::from_websocket(Some(Err(failure))),
        Incoming::from_websocket(None),
    ];

    // Assert
    assert_eq!(
        incoming,
        [
            Incoming::Text("frame".to_string()),
            Incoming::Binary,
            Incoming::Control,
            Incoming::Control,
            Incoming::Closed,
            Incoming::Closed,
            Incoming::Closed,
        ]
    );
}

#[test]
fn close_causes_map_to_websocket_close_codes() {
    // Arrange
    let causes = [CloseCause::Rejected, CloseCause::Stopping];

    // Act
    let codes = causes.map(CloseCause::websocket_code);

    // Assert
    assert_eq!(codes, [close_code::POLICY, close_code::AWAY]);
}
