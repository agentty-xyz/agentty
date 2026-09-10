use ag_session::SessionStatus as Status;

#[test]
fn test_status_transition_question_to_done_rejected() {
    // Arrange
    let current_status = Status::Question;

    // Act
    let can_transition = current_status.can_transition_to(Status::Done);

    // Assert
    assert!(!can_transition);
}
