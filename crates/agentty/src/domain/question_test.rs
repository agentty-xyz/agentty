use ag_session::QuestionItem;

use super::QuestionProgress;
use crate::domain::input::InputState;

#[test]
fn test_applies_to_accepts_matching_progress() {
    // Arrange
    let questions = vec![
        QuestionItem::with_options("Continue?", vec!["Yes".to_string()]),
        QuestionItem::with_options("Scope?", vec!["Small".to_string(), "Large".to_string()]),
    ];
    let progress = QuestionProgress {
        current_index: 1,
        input: InputState::default(),
        responses: vec!["Yes".to_string()],
        selected_option_index: Some(1),
    };

    // Act & Assert
    assert!(progress.applies_to(&questions));
}

#[test]
fn test_applies_to_rejects_response_count_mismatch() {
    // Arrange — one response recorded but the index says none answered.
    let questions = vec![QuestionItem::new("First?"), QuestionItem::new("Second?")];
    let progress = QuestionProgress {
        current_index: 0,
        input: InputState::default(),
        responses: vec!["Yes".to_string()],
        selected_option_index: None,
    };

    // Act & Assert
    assert!(!progress.applies_to(&questions));
}

#[test]
fn test_applies_to_rejects_index_past_question_list() {
    // Arrange — the saved index points past a shrunken question list.
    let questions = vec![QuestionItem::new("Only question?")];
    let progress = QuestionProgress {
        current_index: 1,
        input: InputState::default(),
        responses: vec!["Yes".to_string()],
        selected_option_index: None,
    };

    // Act & Assert
    assert!(!progress.applies_to(&questions));
}

#[test]
fn test_applies_to_rejects_out_of_range_option_index() {
    // Arrange — the highlighted option no longer exists on the current
    // question.
    let questions = vec![QuestionItem::with_options(
        "Continue?",
        vec!["Yes".to_string()],
    )];
    let progress = QuestionProgress {
        current_index: 0,
        input: InputState::default(),
        responses: Vec::new(),
        selected_option_index: Some(1),
    };

    // Act & Assert
    assert!(!progress.applies_to(&questions));
}
