use super::CHUNK_BYTES;
use crate::{Handshake, NoiseError, Session, StaticKeypair};

/// Frame limit used by [`session_pair`].
const MAX_FRAME_BYTES: usize = CHUNK_BYTES * 3;

/// Returns the device and instance sessions of one finished handshake.
fn session_pair() -> (Session, Session) {
    let instance = StaticKeypair::generate().expect("keypair should generate");
    let device = StaticKeypair::generate().expect("keypair should generate");
    let mut initiator =
        Handshake::device_initiator(&device, instance.public()).expect("initiator should build");
    let mut responder = Handshake::device_responder(&instance).expect("responder should build");
    let first = initiator.write_message().expect("first message");
    responder
        .read_message(&first)
        .expect("responder reads first");
    let second = responder.write_message().expect("second message");
    initiator
        .read_message(&second)
        .expect("initiator reads second");

    (
        initiator
            .into_session(MAX_FRAME_BYTES)
            .expect("device session"),
        responder
            .into_session(MAX_FRAME_BYTES)
            .expect("instance session"),
    )
}

/// Encrypts `payload` as one raw Noise message, bypassing frame markers.
fn raw_message(session: &mut Session, payload: &[u8]) -> Vec<u8> {
    let mut message = vec![0; payload.len() + 16];
    let length = session
        .transport
        .write_message(payload, &mut message)
        .expect("raw message should encrypt");
    message.truncate(length);

    message
}

#[test]
fn frames_round_trip_in_both_directions() {
    // Arrange
    let (mut device, mut instance) = session_pair();

    // Act
    let to_instance = device.seal(b"hello").expect("device seals");
    let to_device = instance.seal(b"").expect("instance seals");
    let received_by_instance = instance.open(&to_instance[0]);
    let received_by_device = device.open(&to_device[0]);

    // Assert
    assert_eq!(to_instance.len(), 1);
    assert_eq!(to_device.len(), 1);
    assert!(!to_instance[0].windows(5).any(|window| window == b"hello"));
    assert_eq!(received_by_instance, Ok(Some(b"hello".to_vec())));
    assert_eq!(received_by_device, Ok(Some(Vec::new())));
}

#[test]
fn large_frames_split_into_messages_and_reassemble() {
    // Arrange
    let (mut device, mut instance) = session_pair();
    let frame: Vec<u8> = (0..=u8::MAX).cycle().take(CHUNK_BYTES * 2 + 10).collect();

    // Act
    let messages = device.seal(&frame).expect("device seals");
    let partial = [instance.open(&messages[0]), instance.open(&messages[1])];
    let complete = instance.open(&messages[2]);

    // Assert
    assert_eq!(messages.len(), 3);
    assert_eq!(partial, [Ok(None), Ok(None)]);
    assert_eq!(complete, Ok(Some(frame)));
}

#[test]
fn frames_above_the_limit_are_refused() {
    // Arrange
    let (mut device, mut instance) = session_pair();
    let frame = vec![0; MAX_FRAME_BYTES + 1];

    // Act
    let messages = device.seal(&frame).expect("device seals");
    let results: Vec<_> = messages
        .iter()
        .map(|message| instance.open(message))
        .collect();

    // Assert
    assert_eq!(
        results,
        [Ok(None), Ok(None), Ok(None), Err(NoiseError::FrameTooLarge)]
    );
}

#[test]
fn tampered_and_replayed_messages_fail() {
    // Arrange
    let (mut device, mut instance) = session_pair();
    let messages = device.seal(b"hello").expect("device seals");
    let mut tampered = messages[0].clone();
    tampered[0] ^= 1;

    // Act
    let tampered_result = instance.open(&tampered);
    let first_delivery = instance.open(&messages[0]);
    let replayed = instance.open(&messages[0]);

    // Assert
    assert_eq!(tampered_result, Err(NoiseError::Protocol));
    assert_eq!(first_delivery, Ok(Some(b"hello".to_vec())));
    assert_eq!(replayed, Err(NoiseError::Protocol));
}

#[test]
fn messages_without_a_valid_marker_fail() {
    // Arrange
    let (mut device, mut instance) = session_pair();
    let empty = raw_message(&mut device, &[]);
    let unknown_marker = raw_message(&mut device, &[9, 7]);

    // Act
    let empty_result = instance.open(&empty);
    let unknown_result = instance.open(&unknown_marker);

    // Assert
    assert_eq!(empty_result, Err(NoiseError::Protocol));
    assert_eq!(unknown_result, Err(NoiseError::Protocol));
}
