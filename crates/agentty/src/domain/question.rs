//! Clarification question domain state built on the shared protocol
//! clarification question models re-exported from `ag_protocol`.

pub use ag_protocol::QuestionItem;

use crate::domain::input::InputState;

/// Returns the initial highlighted option for a clarification question.
///
/// Questions with predefined options begin on the first option; free-text
/// questions begin with no highlighted option.
#[must_use]
pub fn default_option_index(questions: &[QuestionItem], question_index: usize) -> Option<usize> {
    questions
        .get(question_index)
        .filter(|item| !item.options.is_empty())
        .map(|_| 0)
}

/// Partially answered clarification state saved when the user leaves question
/// mode for the sessions list.
///
/// Stored per session so already-submitted answers survive exiting with `q`
/// and are restored the next time the session's question mode opens.
pub struct QuestionProgress {
    /// Index of the next unanswered question.
    pub current_index: usize,
    /// Free-text input typed for the current question.
    pub input: InputState,
    /// Responses already submitted for earlier questions.
    pub responses: Vec<String>,
    /// Highlighted predefined option for the current question, or `None`
    /// when the free-text input is active.
    pub selected_option_index: Option<usize>,
}

impl QuestionProgress {
    /// Returns whether this progress still matches the given question list.
    ///
    /// Guards against restoring stale progress after the question set
    /// changed: every stored response must map to an earlier question, the
    /// next question must exist, and any highlighted option must be valid
    /// for that question.
    #[must_use]
    pub fn applies_to(&self, questions: &[QuestionItem]) -> bool {
        if self.responses.len() != self.current_index || self.current_index >= questions.len() {
            return false;
        }

        match self.selected_option_index {
            Some(option_index) => option_index < questions[self.current_index].options.len(),
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
