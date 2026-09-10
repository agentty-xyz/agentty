use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{ConfirmationDecision, NO_OPTION_INDEX, YES_OPTION_INDEX, handle};

#[test]
fn test_handle_returns_confirm_for_yes_shortcut() {
    // Arrange
    let mut selected_confirmation_index = NO_OPTION_INDEX;

    // Act
    let decision = handle(
        &mut selected_confirmation_index,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(decision, ConfirmationDecision::Confirm));
}

#[test]
fn test_handle_returns_reject_for_no_shortcut() {
    // Arrange
    let mut selected_confirmation_index = YES_OPTION_INDEX;

    // Act
    let decision = handle(
        &mut selected_confirmation_index,
        KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(decision, ConfirmationDecision::Reject));
}

#[test]
fn test_handle_returns_cancel_for_escape() {
    // Arrange
    let mut selected_confirmation_index = YES_OPTION_INDEX;

    // Act
    let decision = handle(
        &mut selected_confirmation_index,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(decision, ConfirmationDecision::Cancel));
}

#[test]
fn test_handle_returns_cancel_for_quit_shortcut() {
    // Arrange
    let mut selected_confirmation_index = YES_OPTION_INDEX;

    // Act
    let decision = handle(
        &mut selected_confirmation_index,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(decision, ConfirmationDecision::Cancel));
}

#[test]
fn test_handle_updates_selection_with_arrow_keys() {
    // Arrange
    let mut selected_confirmation_index = YES_OPTION_INDEX;

    // Act
    let move_right_decision = handle(
        &mut selected_confirmation_index,
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
    );
    let move_left_decision = handle(
        &mut selected_confirmation_index,
        KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        move_right_decision,
        ConfirmationDecision::Continue
    ));
    assert!(matches!(move_left_decision, ConfirmationDecision::Continue));
    assert_eq!(selected_confirmation_index, YES_OPTION_INDEX);
}

#[test]
fn test_handle_updates_selection_with_h_and_l_shortcuts() {
    // Arrange
    let mut selected_confirmation_index = YES_OPTION_INDEX;

    // Act
    let move_right_decision = handle(
        &mut selected_confirmation_index,
        KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
    );
    let move_left_decision = handle(
        &mut selected_confirmation_index,
        KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(
        move_right_decision,
        ConfirmationDecision::Continue
    ));
    assert!(matches!(move_left_decision, ConfirmationDecision::Continue));
    assert_eq!(selected_confirmation_index, YES_OPTION_INDEX);
}

#[test]
fn test_handle_enter_uses_selected_option() {
    // Arrange
    let mut yes_selected_index = YES_OPTION_INDEX;
    let mut no_selected_index = NO_OPTION_INDEX;

    // Act
    let yes_decision = handle(
        &mut yes_selected_index,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    let no_decision = handle(
        &mut no_selected_index,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );

    // Assert
    assert!(matches!(yes_decision, ConfirmationDecision::Confirm));
    assert!(matches!(no_decision, ConfirmationDecision::Reject));
}
