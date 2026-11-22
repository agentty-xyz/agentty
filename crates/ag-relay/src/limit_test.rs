use std::time::Duration;

use super::ByteBudget;
use crate::RelayLimits;

#[test]
fn default_limits_bound_every_resource() {
    // Arrange
    let limits = RelayLimits::default();

    // Act
    let bounded = [
        limits.max_bytes_per_minute,
        limits.max_handshakes,
        limits.max_instances,
        limits.max_instances_per_address,
        limits.max_streams_per_instance,
    ];

    // Assert
    assert!(bounded.iter().all(|limit| *limit > 0));
    assert!(limits.accept_timeout > Duration::ZERO);
    assert!(limits.handshake_timeout > Duration::ZERO);
    assert!(limits.ping_interval > Duration::ZERO);
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
