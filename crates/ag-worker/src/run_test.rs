//! `RunState` storage name and `RunRepository` default unit tests.

use ag_contracts::UsagePurpose;
use ag_contracts::test_support::token_usage;

use crate::run::mock::MockRunRepository;
use crate::run::{RunRepository, RunState};

#[test]
fn run_states_use_stable_storage_names() {
    // Arrange
    let states = [
        RunState::Queued,
        RunState::Running,
        RunState::Completed,
        RunState::Failed,
        RunState::Canceled,
    ];

    // Act
    let names = states.map(RunState::as_str);

    // Assert
    assert_eq!(
        names,
        ["queued", "running", "completed", "failed", "canceled"]
    );
}

#[tokio::test]
async fn repositories_that_keep_no_usage_accept_and_ignore_it() {
    // Arrange
    let repository = MockRunRepository::new();

    // Act
    let result = repository
        .record_usage("session", UsagePurpose::Title, &token_usage("model", 3, 5))
        .await;

    // Assert
    assert!(result.is_ok());
}
