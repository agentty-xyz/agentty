use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use super::{ByteBudget, HandshakeLimiter};
use crate::RelayLimits;

/// Time Agentty waits for the relay to confirm a registration.
const AGENTTY_REGISTRATION_WAIT: Duration = Duration::from_secs(10);

#[test]
fn default_limits_bound_every_resource() {
    // Arrange
    let limits = RelayLimits::default();

    // Act
    let bounded = [
        limits.max_bytes_per_minute,
        limits.max_handshakes,
        limits.max_handshakes_per_address,
        limits.max_instances,
        limits.max_instances_per_address,
        limits.max_streams_per_instance,
    ];
    let durations = [
        limits.accept_timeout,
        limits.admission_timeout,
        limits.handshake_timeout,
        limits.ping_interval,
        limits.shutdown_timeout,
        limits.write_timeout,
    ];

    // Assert
    assert!(bounded.iter().all(|limit| *limit > 0));
    assert!(durations.iter().all(|duration| *duration > Duration::ZERO));
    assert!(limits.max_handshakes_per_address < limits.max_handshakes);
    assert!(limits.admission_timeout < AGENTTY_REGISTRATION_WAIT);
}

#[test]
fn handshake_reservations_stop_at_the_address_and_total_limits() {
    // Arrange
    let limits = RelayLimits {
        max_handshakes: 3,
        max_handshakes_per_address: 2,
        ..RelayLimits::default()
    };
    let limiter = Arc::new(HandshakeLimiter::new(&limits));
    let first_address = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
    let second_address = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2));

    // Act
    let first = limiter.reserve(first_address);
    let second = limiter.reserve(first_address);
    let over_address_limit = limiter.reserve(first_address).is_none();
    let third = limiter.reserve(second_address);
    let over_total_limit = limiter.reserve(second_address).is_none();
    drop(first);
    let after_one_release = limiter.reserve(second_address);
    drop(second);
    let after_address_emptied = limiter.reserve(first_address);

    // Assert
    assert!(over_address_limit);
    assert!(third.is_some());
    assert!(over_total_limit);
    assert!(after_one_release.is_some());
    assert!(after_address_emptied.is_some());
}

#[tokio::test(start_paused = true)]
async fn budget_refuses_bytes_above_the_limit_until_the_window_ends() {
    // Arrange
    let mut budget = ByteBudget::new(10);

    // Act
    let within = budget.spend(10);
    let above = budget.spend(1);
    tokio::time::advance(Duration::from_secs(59)).await;
    let same_window = budget.spend(0);
    tokio::time::advance(Duration::from_secs(1)).await;
    let next_window = budget.spend(10);

    // Assert
    assert!(within);
    assert!(!above);
    assert!(!same_window);
    assert!(next_window);
}
