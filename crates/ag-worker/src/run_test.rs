use crate::run::RunState;

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
