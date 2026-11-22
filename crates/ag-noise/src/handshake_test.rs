use crate::{Handshake, NoiseError, StaticKeypair};

fn keypair() -> StaticKeypair {
    StaticKeypair::generate().expect("keypair should generate")
}

#[test]
fn device_handshake_exchanges_keys_in_two_messages() {
    // Arrange
    let instance = keypair();
    let device = keypair();
    let mut initiator =
        Handshake::device_initiator(&device, instance.public()).expect("initiator should build");
    let mut responder = Handshake::device_responder(&instance).expect("responder should build");

    // Act
    let first = initiator.write_message().expect("first message");
    let finished_after_first = initiator.is_finished();
    responder
        .read_message(&first)
        .expect("responder reads first");
    let second = responder.write_message().expect("second message");
    initiator
        .read_message(&second)
        .expect("initiator reads second");

    // Assert
    assert!(!finished_after_first);
    assert!(initiator.is_finished());
    assert!(responder.is_finished());
    assert_eq!(responder.remote_public_key(), Some(*device.public()));
    assert_eq!(initiator.remote_public_key(), Some(*instance.public()));
}

#[test]
fn device_handshake_fails_against_another_instance_key() {
    // Arrange
    let instance = keypair();
    let other_instance = keypair();
    let device = keypair();
    let mut initiator = Handshake::device_initiator(&device, other_instance.public())
        .expect("initiator should build");
    let mut responder = Handshake::device_responder(&instance).expect("responder should build");

    // Act
    let first = initiator.write_message().expect("first message");
    let result = responder.read_message(&first);

    // Assert
    assert_eq!(result, Err(NoiseError::Protocol));
}

#[test]
fn relay_handshake_proves_the_instance_key_after_three_messages() {
    // Arrange
    let instance = keypair();
    let mut initiator = Handshake::relay_initiator(&instance).expect("initiator should build");
    let mut responder = Handshake::relay_responder().expect("responder should build");

    // Act
    let first = initiator.write_message().expect("first message");
    responder.read_message(&first).expect("relay reads first");
    let second = responder.write_message().expect("second message");
    initiator
        .read_message(&second)
        .expect("instance reads second");
    let key_before_third = responder.remote_public_key();
    let third = initiator.write_message().expect("third message");
    responder.read_message(&third).expect("relay reads third");

    // Assert
    assert_eq!(key_before_third, None);
    assert!(responder.is_finished());
    assert_eq!(responder.remote_public_key(), Some(*instance.public()));
}

#[test]
fn failed_key_proof_reports_no_key_and_spends_the_handshake() {
    // Arrange: the last byte belongs to the proof that follows the
    // encrypted key, so the key itself still decrypts.
    let instance = keypair();
    let mut initiator = Handshake::relay_initiator(&instance).expect("initiator should build");
    let mut responder = Handshake::relay_responder().expect("responder should build");
    let first = initiator.write_message().expect("first message");
    responder.read_message(&first).expect("relay reads first");
    let second = responder.write_message().expect("second message");
    initiator
        .read_message(&second)
        .expect("instance reads second");
    let third = initiator.write_message().expect("third message");
    let mut unproven = third.clone();
    *unproven.last_mut().expect("message should not be empty") ^= 1;

    // Act
    let result = responder.read_message(&unproven);
    let key = responder.remote_public_key();
    let retry = responder.read_message(&third);
    let write = responder.write_message();
    let session = responder.into_session(1024);

    // Assert
    assert_eq!(result, Err(NoiseError::Protocol));
    assert_eq!(key, None);
    assert_eq!(retry, Err(NoiseError::Protocol));
    assert_eq!(write, Err(NoiseError::Protocol));
    assert!(matches!(session, Err(NoiseError::Protocol)));
}

#[test]
fn replayed_device_message_yields_a_session_that_cannot_be_used() {
    // Arrange: a third party replays a captured first message. The instance
    // finishes the handshake but the replayer holds no session keys.
    let instance = keypair();
    let device = keypair();
    let mut initiator =
        Handshake::device_initiator(&device, instance.public()).expect("initiator should build");
    let captured = initiator.write_message().expect("first message");
    let mut responder = Handshake::device_responder(&instance).expect("responder should build");
    let mut replay_target = Handshake::device_responder(&instance).expect("responder should build");
    responder.read_message(&captured).expect("first delivery");
    let second = responder.write_message().expect("second message");
    initiator
        .read_message(&second)
        .expect("initiator reads second");
    let mut device_session = initiator.into_session(1024).expect("device session");

    // Act
    replay_target.read_message(&captured).expect("replay reads");
    replay_target.write_message().expect("replay answers");
    let replayed_key = replay_target.remote_public_key();
    let mut replayed_session = replay_target.into_session(1024).expect("replayed session");
    let frame = device_session.seal(b"hello").expect("device seals");
    let opened = replayed_session.open(&frame[0]);

    // Assert
    assert_eq!(replayed_key, Some(*device.public()));
    assert_eq!(opened, Err(NoiseError::Protocol));
}

#[test]
fn writing_or_finishing_out_of_turn_fails() {
    // Arrange
    let instance = keypair();
    let mut responder = Handshake::device_responder(&instance).expect("responder should build");
    let unfinished = Handshake::relay_responder().expect("responder should build");

    // Act
    let out_of_turn = responder.write_message();
    let session = unfinished.into_session(1024);

    // Assert
    assert_eq!(out_of_turn, Err(NoiseError::Protocol));
    assert!(matches!(session, Err(NoiseError::Protocol)));
}
