use ag_noise::{Handshake, Session, StaticKeypair};
use tokio::sync::mpsc;

use super::{EncryptedTransport, MAX_FRAME_BYTES, MessageCarrier};
use crate::transport::{FrameTransport, Incoming};

/// One end of an in-memory carrier.
struct ChannelCarrier {
    incoming: mpsc::UnboundedReceiver<Vec<u8>>,
    outgoing: mpsc::UnboundedSender<Vec<u8>>,
}

impl ChannelCarrier {
    /// Returns the two ends of one carrier.
    fn pair() -> (Self, Self) {
        let (first_tx, first_rx) = mpsc::unbounded_channel();
        let (second_tx, second_rx) = mpsc::unbounded_channel();

        (
            Self {
                incoming: first_rx,
                outgoing: second_tx,
            },
            Self {
                incoming: second_rx,
                outgoing: first_tx,
            },
        )
    }
}

impl MessageCarrier for ChannelCarrier {
    async fn receive(&mut self) -> Option<Vec<u8>> {
        self.incoming.recv().await
    }

    async fn send(&mut self, message: Vec<u8>) {
        let _ = self.outgoing.send(message);
    }

    async fn close(&mut self) {
        self.incoming.close();
    }
}

/// A device with a finished handshake against an accepted transport.
struct PairedEnds {
    device: ChannelCarrier,
    device_key: StaticKeypair,
    session: Session,
    transport: EncryptedTransport<ChannelCarrier>,
}

/// Runs the device handshake between a device and an accepted transport.
async fn paired_ends() -> PairedEnds {
    let instance_key = StaticKeypair::generate().expect("instance key");
    let device_key = StaticKeypair::generate().expect("device key");
    let (mut device, instance) = ChannelCarrier::pair();
    let mut handshake =
        Handshake::device_initiator(&device_key, instance_key.public()).expect("initiator");
    device
        .send(handshake.write_message().expect("first message"))
        .await;
    let (transport, named_key) = EncryptedTransport::accept(instance, &instance_key)
        .await
        .expect("transport should accept the device");
    let second = device.receive().await.expect("second message");
    handshake
        .read_message(&second)
        .expect("device reads second");
    let session = handshake
        .into_session(MAX_FRAME_BYTES)
        .expect("device session");
    assert_eq!(named_key, *device_key.public());

    PairedEnds {
        device,
        device_key,
        session,
        transport,
    }
}

/// Encrypts `frame` on the device and sends every message.
async fn send_from_device(ends: &mut PairedEnds, frame: &[u8]) {
    for message in ends.session.seal(frame).expect("device seals") {
        ends.device.send(message).await;
    }
}

#[tokio::test]
async fn accepted_transport_exchanges_text_frames_with_the_device() {
    // Arrange
    let mut ends = paired_ends().await;
    let large_frame = "x".repeat(200_000);

    // Act
    send_from_device(&mut ends, b"hello").await;
    let received = ends.transport.receive().await;
    send_from_device(&mut ends, large_frame.as_bytes()).await;
    let received_large = ends.transport.receive().await;
    ends.transport.send("reply".to_string()).await;
    let reply = ends.device.receive().await.expect("reply message");
    let opened_reply = ends.session.open(&reply);

    // Assert
    assert_eq!(received, Incoming::Text("hello".to_string()));
    assert_eq!(received_large, Incoming::Text(large_frame));
    assert_eq!(opened_reply, Ok(Some(b"reply".to_vec())));
    assert_eq!(ends.device_key.public().as_bytes().len(), 32);
}

#[tokio::test]
async fn frames_that_are_not_text_arrive_as_binary() {
    // Arrange
    let mut ends = paired_ends().await;

    // Act
    send_from_device(&mut ends, &[0xFF, 0xFE]).await;
    let received = ends.transport.receive().await;

    // Assert
    assert_eq!(received, Incoming::Binary);
}

#[tokio::test]
async fn tampered_messages_and_a_closed_carrier_end_the_transport() {
    // Arrange
    let mut tampered_ends = paired_ends().await;
    let mut closed_ends = paired_ends().await;
    let mut messages = tampered_ends.session.seal(b"hello").expect("device seals");
    messages[0][0] ^= 1;

    // Act
    tampered_ends.device.send(messages.remove(0)).await;
    let after_tampering = tampered_ends.transport.receive().await;
    drop(closed_ends.device);
    let after_closing = closed_ends.transport.receive().await;

    // Assert
    assert_eq!(after_tampering, Incoming::Closed);
    assert_eq!(after_closing, Incoming::Closed);
}

#[tokio::test]
async fn closing_the_transport_closes_the_carrier() {
    // Arrange
    let mut ends = paired_ends().await;
    send_from_device(&mut ends, b"queued").await;

    // Act
    ends.transport.close().await;
    let queued = ends.transport.receive().await;
    let after_queue = ends.transport.receive().await;

    // Assert
    assert_eq!(queued, Incoming::Text("queued".to_string()));
    assert_eq!(after_queue, Incoming::Closed);
}

#[tokio::test]
async fn accept_refuses_a_broken_handshake_or_a_closed_carrier() {
    // Arrange
    let instance_key = StaticKeypair::generate().expect("instance key");
    let (mut garbage_device, garbage_instance) = ChannelCarrier::pair();
    let (closed_device, closed_instance) = ChannelCarrier::pair();
    garbage_device.send(vec![1, 2, 3]).await;
    drop(closed_device);

    // Act
    let garbage = EncryptedTransport::accept(garbage_instance, &instance_key).await;
    let closed = EncryptedTransport::accept(closed_instance, &instance_key).await;

    // Assert
    assert!(garbage.is_none());
    assert!(closed.is_none());
}
