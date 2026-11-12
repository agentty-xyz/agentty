use ag_session::{QuestionAnswer, QuestionItem};

use super::{QuestionProgress, question_answer_message};
use crate::domain::input::InputState;

#[test]
fn question_answer_message_pairs_each_question_with_its_answer() {
    // Arrange
    let answers = [
        QuestionAnswer {
            answer: "main".to_string(),
            question: "Which target?".to_string(),
        },
        QuestionAnswer {
            answer: "yes".to_string(),
            question: "Run tests?".to_string(),
        },
    ];

    // Act
    let message = question_answer_message(&answers);

    // Assert
    assert_eq!(
        message,
        "Clarifications:\n1. Q: Which target?\n   A: main\n2. Q: Run tests?\n   A: yes"
    );
}

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
