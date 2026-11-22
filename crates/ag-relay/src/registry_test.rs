use std::net::{IpAddr, Ipv4Addr};

use super::Registry;
use crate::RelayLimits;
use crate::close::CloseReason;

/// Address most tests register from.
const ADDRESS: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
/// A second client address.
const OTHER_ADDRESS: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2));

/// Limits small enough to reach in a test.
fn limits() -> RelayLimits {
    RelayLimits {
        max_instances: 3,
        max_instances_per_address: 2,
        max_streams_per_instance: 2,
        ..RelayLimits::default()
    }
}

#[test]
fn open_stream_notifies_the_instance_and_delivers_its_answer() {
    // Arrange
    let registry = Registry::<u8>::new();
    let mut registration = registry
        .register("instance", ADDRESS, &limits())
        .expect("instance should register");

    // Act
    let mut answer = registry
        .open_stream("instance", "stream", &limits())
        .expect("stream should open");
    let notified = registration.streams.try_recv();
    let was_pending = registry.has_pending("stream");
    let sender = registry.accept_stream("stream");
    let second_accept = registry.accept_stream("stream");
    sender
        .expect("stream should be pending")
        .send(7)
        .expect("answer should deliver");

    // Assert
    assert_eq!(notified, Ok("stream".to_string()));
    assert!(was_pending);
    assert!(second_accept.is_none());
    assert!(!registry.has_pending("stream"));
    assert_eq!(answer.try_recv(), Ok(7));
}

#[test]
fn open_stream_refuses_unknown_busy_and_full_instances() {
    // Arrange
    let registry = Registry::<u8>::new();
    let mut registration = registry
        .register("instance", ADDRESS, &limits())
        .expect("instance should register");
    let gone = registry
        .register("gone", ADDRESS, &limits())
        .expect("instance should register");
    drop(gone.streams);

    // Act
    let unknown = registry.open_stream("missing", "a", &limits()).err();
    let gone_busy = registry.open_stream("gone", "b", &limits()).err();
    let first = registry.open_stream("instance", "c", &limits()).is_ok();
    let second = registry.open_stream("instance", "d", &limits()).is_ok();
    let full = registry.open_stream("instance", "e", &limits()).err();
    registry.close_stream("instance", "c");
    let unread_notices = registry.open_stream("instance", "f", &limits()).err();
    let read = [
        registration.streams.try_recv(),
        registration.streams.try_recv(),
    ];
    let after_reading = registry.open_stream("instance", "g", &limits()).is_ok();

    // Assert
    assert_eq!(unknown, Some(CloseReason::UnknownInstance));
    assert_eq!(gone_busy, Some(CloseReason::InstanceBusy));
    assert!(first && second);
    assert_eq!(full, Some(CloseReason::TooManyStreams));
    assert_eq!(unread_notices, Some(CloseReason::InstanceBusy));
    assert_eq!(read, [Ok("c".to_string()), Ok("d".to_string())]);
    assert!(after_reading);
    assert!(!registry.has_pending("c"));
}

#[test]
fn closing_every_stream_frees_the_instance_for_new_ones() {
    // Arrange
    let registry = Registry::<u8>::new();
    let mut registration = registry
        .register("instance", ADDRESS, &limits())
        .expect("instance should register");
    let first = registry.open_stream("instance", "a", &limits()).is_ok();
    let second = registry.open_stream("instance", "b", &limits()).is_ok();
    let notices = [
        registration.streams.try_recv(),
        registration.streams.try_recv(),
    ];

    // Act
    registry.close_stream("instance", "a");
    registry.close_stream("instance", "b");
    registry.close_stream("instance", "never-opened");
    let reopened = [
        registry.open_stream("instance", "c", &limits()).is_ok(),
        registry.open_stream("instance", "d", &limits()).is_ok(),
    ];

    // Assert
    assert!(first && second);
    assert_eq!(notices, [Ok("a".to_string()), Ok("b".to_string())]);
    assert_eq!(reopened, [true, true]);
}

#[test]
fn register_limits_instances_in_total_and_per_address() {
    // Arrange
    let registry = Registry::<u8>::new();
    let limits = limits();

    // Act
    let first = registry.register("a", ADDRESS, &limits).is_ok();
    let second = registry.register("b", ADDRESS, &limits).is_ok();
    let third_from_address = registry.register("c", ADDRESS, &limits).err();
    let third = registry.register("c", OTHER_ADDRESS, &limits).is_ok();
    let fourth = registry.register("d", OTHER_ADDRESS, &limits).err();

    // Assert
    assert!(first && second && third);
    assert_eq!(third_from_address, Some(CloseReason::TooManyInstances));
    assert_eq!(fourth, Some(CloseReason::RelayFull));
}

#[test]
fn registering_again_replaces_the_earlier_registration() {
    // Arrange
    let registry = Registry::<u8>::new();
    let limits = RelayLimits {
        max_instances: 1,
        max_instances_per_address: 1,
        ..limits()
    };
    let mut earlier = registry
        .register("instance", ADDRESS, &limits)
        .expect("instance should register");

    // Act
    let mut newer = registry
        .register("instance", ADDRESS, &limits)
        .expect("instance should register again");
    let earlier_ended = earlier.streams.try_recv().is_err();
    registry.unregister("instance", earlier.generation);
    let kept_after_stale_unregister = registry.has_instance("instance");
    let opened = registry.open_stream("instance", "stream", &limits).is_ok();
    let notified = newer.streams.try_recv();
    registry.unregister("instance", newer.generation);

    // Assert
    assert!(earlier_ended);
    assert!(kept_after_stale_unregister);
    assert!(opened);
    assert_eq!(notified, Ok("stream".to_string()));
    assert!(!registry.has_instance("instance"));
}
