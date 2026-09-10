use crate::status::{
    validate_operation, validate_orchestration, validate_orchestration_task, validate_session,
};

#[test]
fn codecs_accept_current_and_legacy_statuses() {
    // Arrange / Act / Assert
    assert!(validate_operation("queued").is_ok());
    assert!(validate_orchestration("Running").is_ok());
    assert!(validate_orchestration_task("WaitingForInput").is_ok());
    assert!(validate_session("Committing").is_ok());
}

#[test]
fn codecs_reject_unknown_statuses_with_entity_context() {
    // Arrange
    let statuses = [
        validate_operation("unknown"),
        validate_orchestration("unknown"),
        validate_orchestration_task("unknown"),
        validate_session("unknown"),
    ];

    // Act
    let messages =
        statuses.map(|result| result.expect_err("unknown status should fail").to_string());

    // Assert
    assert_eq!(
        messages,
        [
            "Invalid session operation lifecycle status `unknown`",
            "Invalid orchestration lifecycle status `unknown`",
            "Invalid orchestration task lifecycle status `unknown`",
            "Invalid session lifecycle status `unknown`",
        ]
    );
}
