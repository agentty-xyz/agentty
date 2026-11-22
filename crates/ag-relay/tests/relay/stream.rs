//! Streams between a device and the connection its instance dials.

use std::time::Duration;

use ag_relay::{ACCEPT_PATH_PREFIX, CONNECT_PATH_PREFIX, RelayLimits};
use futures_util::SinkExt;
use tokio_tungstenite::tungstenite::Message;

use crate::support::{
    TestResult, accept_next_stream, connect_device, joined_stream, keypair, next_binary,
    next_close, next_ping, next_stream_id, open, refused_status, register, send_binary, start,
};

#[tokio::test]
async fn joined_stream_forwards_binary_messages_both_ways() -> TestResult {
    // Arrange
    let relay = start(RelayLimits::default()).await?;
    let instance_key = keypair()?;
    let mut control = register(&relay, &instance_key).await?;
    let mut device = connect_device(&relay, &instance_key).await?;

    // Act: the device may send before its instance answers.
    send_binary(&mut device, b"early".to_vec()).await?;
    let mut instance = accept_next_stream(&relay, &mut control).await?;
    let early = next_binary(&mut instance).await?;
    send_binary(&mut instance, b"from instance".to_vec()).await?;
    device.send(Message::Ping(Vec::new().into())).await?;
    send_binary(&mut device, b"from device".to_vec()).await?;
    let to_device = next_binary(&mut device).await?;
    let to_instance = next_binary(&mut instance).await?;

    // Assert
    assert_eq!(early, b"early");
    assert_eq!(to_device, b"from instance");
    assert_eq!(to_instance, b"from device");

    Ok(())
}

#[tokio::test]
async fn unknown_instances_and_streams_are_refused() -> TestResult {
    // Arrange
    let relay = start(RelayLimits::default()).await?;
    let instance_key = keypair()?;
    let mut control = register(&relay, &instance_key).await?;
    let _device = connect_device(&relay, &instance_key).await?;
    let stream_id = next_stream_id(&mut control).await?;
    let accept_path = format!("{ACCEPT_PATH_PREFIX}{stream_id}");

    // Act
    let unknown_instance = refused_status(&relay, &format!("{CONNECT_PATH_PREFIX}missing")).await?;
    let unknown_stream = refused_status(&relay, &format!("{ACCEPT_PATH_PREFIX}missing")).await?;
    let _instance = open(&relay, &accept_path).await?;
    let answered_twice = refused_status(&relay, &accept_path).await?;

    // Assert
    assert_eq!(unknown_instance, 404);
    assert_eq!(unknown_stream, 404);
    assert_eq!(answered_twice, 404);

    Ok(())
}

#[tokio::test]
async fn unanswered_stream_is_closed_and_frees_its_slot() -> TestResult {
    // Arrange
    let limits = RelayLimits {
        accept_timeout: Duration::from_millis(50),
        max_streams_per_instance: 1,
        ..RelayLimits::default()
    };
    let relay = start(limits).await?;
    let instance_key = keypair()?;
    let mut control = register(&relay, &instance_key).await?;

    // Act
    let mut unanswered = connect_device(&relay, &instance_key).await?;
    let mut over_limit = connect_device(&relay, &instance_key).await?;
    let over_limit_close = next_close(&mut over_limit).await?;
    let unanswered_close = next_close(&mut unanswered).await?;
    let stale_stream = next_stream_id(&mut control).await?;
    let stale_status =
        refused_status(&relay, &format!("{ACCEPT_PATH_PREFIX}{stale_stream}")).await?;
    let mut device = connect_device(&relay, &instance_key).await?;
    let mut instance = accept_next_stream(&relay, &mut control).await?;
    send_binary(&mut device, b"after".to_vec()).await?;
    let forwarded = next_binary(&mut instance).await?;

    // Assert
    assert_eq!(
        over_limit_close,
        (1008, "Too many streams for this instance".to_string())
    );
    assert_eq!(
        unanswered_close,
        (1001, "Instance did not answer".to_string())
    );
    assert_eq!(stale_status, 404);
    assert_eq!(forwarded, b"after");

    Ok(())
}

#[tokio::test]
async fn leaving_side_closes_the_other_side() -> TestResult {
    // Arrange
    let relay = start(RelayLimits::default()).await?;
    let (_control, mut device, mut instance) = joined_stream(&relay).await?;
    let (_other_control, mut other_device, mut other_instance) = joined_stream(&relay).await?;

    // Act
    device.close(None).await?;
    let instance_close = next_close(&mut instance).await?;
    drop(other_instance.close(None).await);
    let device_close = next_close(&mut other_device).await?;

    // Assert
    assert_eq!(instance_close, (1000, "Peer left".to_string()));
    assert_eq!(device_close, (1000, "Peer left".to_string()));

    Ok(())
}

#[tokio::test]
async fn text_messages_end_the_stream_for_both_sides() -> TestResult {
    // Arrange
    let relay = start(RelayLimits::default()).await?;
    let (_control, mut device, mut instance) = joined_stream(&relay).await?;

    // Act
    device.send(Message::Text("plain".into())).await?;
    let device_close = next_close(&mut device).await?;
    let instance_close = next_close(&mut instance).await?;

    // Assert
    let expected = (1008, "Text messages are not supported".to_string());
    assert_eq!(device_close, expected);
    assert_eq!(instance_close, expected);

    Ok(())
}

#[tokio::test]
async fn streams_above_their_byte_budget_are_closed() -> TestResult {
    // Arrange
    let limits = RelayLimits {
        max_bytes_per_minute: 8,
        ..RelayLimits::default()
    };
    let relay = start(limits).await?;
    let (_control, mut device, mut instance) = joined_stream(&relay).await?;

    // Act
    send_binary(&mut device, vec![0; 5]).await?;
    let within_budget = next_binary(&mut instance).await?;
    send_binary(&mut instance, vec![0; 4]).await?;
    let device_close = next_close(&mut device).await?;
    let instance_close = next_close(&mut instance).await?;

    // Assert
    assert_eq!(within_budget.len(), 5);
    assert_eq!(device_close, (1008, "Rate limit exceeded".to_string()));
    assert_eq!(instance_close, (1008, "Rate limit exceeded".to_string()));

    Ok(())
}

#[tokio::test]
async fn joined_streams_get_keepalive_pings() -> TestResult {
    // Arrange
    let limits = RelayLimits {
        ping_interval: Duration::from_millis(20),
        ..RelayLimits::default()
    };
    let relay = start(limits).await?;
    let (_control, mut device, mut instance) = joined_stream(&relay).await?;

    // Act
    next_ping(&mut device).await?;
    next_ping(&mut instance).await?;
    send_binary(&mut device, b"still open".to_vec()).await?;
    let forwarded = next_binary(&mut instance).await?;

    // Assert
    assert_eq!(forwarded, b"still open");

    Ok(())
}
