//! Registration of instances on their control connection.

use std::time::Duration;

use ag_relay::{CONNECT_PATH_PREFIX, REGISTER_PATH, RelayLimits};
use futures_util::SinkExt;
use tokio_tungstenite::tungstenite::Message;

use crate::support::{
    TestResult, connect_device, keypair, next_binary, next_close, next_ping, next_stream_id,
    next_text, open, open_control_from, prove_key, refused_status, register, send_binary, start,
    start_with_header,
};

#[tokio::test]
async fn registered_instance_is_told_about_each_device_stream() -> TestResult {
    // Arrange
    let relay = start(RelayLimits::default()).await?;
    let instance = keypair()?;
    let mut control = register(&relay, &instance).await?;

    // Act
    let _first_device = connect_device(&relay, &instance).await?;
    let first = next_stream_id(&mut control).await?;
    let _second_device = connect_device(&relay, &instance).await?;
    let second = next_stream_id(&mut control).await?;

    // Assert
    assert_eq!(first.len(), 32);
    assert_ne!(first, second);

    Ok(())
}

#[tokio::test]
async fn control_connection_ignores_instance_messages_and_gets_pings() -> TestResult {
    // Arrange
    let limits = RelayLimits {
        ping_interval: Duration::from_millis(20),
        ..RelayLimits::default()
    };
    let relay = start(limits).await?;
    let instance = keypair()?;
    let mut control = register(&relay, &instance).await?;

    // Act
    control.send(Message::Text("ignored".into())).await?;
    next_ping(&mut control).await?;
    let _device = connect_device(&relay, &instance).await?;
    let stream_id = next_stream_id(&mut control).await?;

    // Assert
    assert_ne!(stream_id, "");

    Ok(())
}

#[tokio::test]
async fn leaving_instance_is_unregistered() -> TestResult {
    // Arrange
    let relay = start(RelayLimits::default()).await?;
    let instance = keypair()?;
    let mut control = register(&relay, &instance).await?;
    let path = format!("{CONNECT_PATH_PREFIX}{}", instance.public().fingerprint());

    // Act
    control.close(None).await?;
    let mut status = 0;
    for _ in 0..50 {
        status = refused_status(&relay, &path).await.unwrap_or(0);
        if status == 404 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Assert
    assert_eq!(status, 404);

    Ok(())
}

#[tokio::test]
async fn failed_handshakes_are_closed_without_registering() -> TestResult {
    // Arrange
    let relay = start(RelayLimits::default()).await?;
    let instance = keypair()?;
    let mut garbage = open(&relay, REGISTER_PATH).await?;
    let mut text = open(&relay, REGISTER_PATH).await?;
    let mut unproven = open(&relay, REGISTER_PATH).await?;

    // Act
    send_binary(&mut garbage, vec![1, 2, 3]).await?;
    text.send(Message::Text("hello".into())).await?;
    let mut handshake = ag_noise::Handshake::relay_initiator(&instance)?;
    send_binary(&mut unproven, handshake.write_message()?).await?;
    handshake.read_message(&next_binary(&mut unproven).await?)?;
    let mut third = handshake.write_message()?;
    *third.last_mut().ok_or("empty message")? ^= 1;
    send_binary(&mut unproven, third).await?;
    let closes = [
        next_close(&mut garbage).await?,
        next_close(&mut text).await?,
        next_close(&mut unproven).await?,
    ];
    let path = format!("{CONNECT_PATH_PREFIX}{}", instance.public().fingerprint());
    let status = refused_status(&relay, &path).await?;

    // Assert
    for close in closes {
        assert_eq!(close, (1008, "Handshake failed".to_string()));
    }
    assert_eq!(status, 404);

    Ok(())
}

#[tokio::test]
async fn silent_instances_are_closed_after_the_handshake_timeout() -> TestResult {
    // Arrange
    let limits = RelayLimits {
        handshake_timeout: Duration::from_millis(50),
        ..RelayLimits::default()
    };
    let relay = start(limits).await?;

    // Act
    let mut silent = open(&relay, REGISTER_PATH).await?;
    let close = next_close(&mut silent).await?;

    // Assert
    assert_eq!(close, (1008, "Handshake failed".to_string()));

    Ok(())
}

#[tokio::test]
async fn handshakes_in_progress_are_capped() -> TestResult {
    // Arrange
    let limits = RelayLimits {
        max_handshakes: 1,
        ..RelayLimits::default()
    };
    let relay = start(limits).await?;
    let instance = keypair()?;

    // Act
    let mut first = open(&relay, REGISTER_PATH).await?;
    let refused = refused_status(&relay, REGISTER_PATH).await?;
    prove_key(&mut first, &instance).await?;
    let confirmation = next_text(&mut first).await?;
    let after_handshake = open(&relay, REGISTER_PATH).await;

    // Assert
    assert_eq!(refused, 503);
    assert_eq!(confirmation, "registered");
    assert!(after_handshake.is_ok());

    Ok(())
}

#[tokio::test]
async fn registering_again_replaces_the_earlier_control_connection() -> TestResult {
    // Arrange
    let relay = start(RelayLimits::default()).await?;
    let instance = keypair()?;
    let mut earlier = register(&relay, &instance).await?;

    // Act
    let mut newer = register(&relay, &instance).await?;
    let close = next_close(&mut earlier).await?;
    let _device = connect_device(&relay, &instance).await?;
    let stream_id = next_stream_id(&mut newer).await?;

    // Assert
    assert_eq!(
        close,
        (1008, "Replaced by a newer registration".to_string())
    );
    assert_ne!(stream_id, "");

    Ok(())
}

#[tokio::test]
async fn instance_limits_close_extra_registrations() -> TestResult {
    // Arrange
    let limits = RelayLimits {
        max_instances_per_address: 1,
        ..RelayLimits::default()
    };
    let relay = start_with_header(limits, Some("x-real-ip")).await?;
    let _first = register(&relay, &keypair()?).await?;

    // Act
    let mut same_address = open(&relay, REGISTER_PATH).await?;
    prove_key(&mut same_address, &keypair()?).await?;
    let close = next_close(&mut same_address).await?;
    let mut other_address = open_control_from(&relay, "203.0.113.9").await?;
    prove_key(&mut other_address, &keypair()?).await?;
    let confirmation = next_text(&mut other_address).await?;

    // Assert
    assert_eq!(
        close,
        (1008, "Too many instances for this address".to_string())
    );
    assert_eq!(confirmation, "registered");

    Ok(())
}
