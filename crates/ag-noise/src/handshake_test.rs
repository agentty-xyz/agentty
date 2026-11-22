use crate::{Handshake, NoiseError, StaticKeypair};

fn keypair() -> StaticKeypair {
    StaticKeypair::generate().expect("keypair should generate")
}

#[test]
fn device_handshake_authenticates_both_sides_in_two_messages() {
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
fn relay_handshake_rejects_a_tampered_key_proof() {
    // Arrange
    let instance = keypair();
    let mut initiator = Handshake::relay_initiator(&instance).expect("initiator should build");
    let mut responder = Handshake::relay_responder().expect("responder should build");
    let first = initiator.write_message().expect("first message");
    responder.read_message(&first).expect("relay reads first");
    let second = responder.write_message().expect("second message");
    initiator
        .read_message(&second)
        .expect("instance reads second");
    let mut third = initiator.write_message().expect("third message");
    third[0] ^= 1;

    // Act
    let result = responder.read_message(&third);

    // Assert
    assert_eq!(result, Err(NoiseError::Protocol));
    assert_eq!(responder.remote_public_key(), None);
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
