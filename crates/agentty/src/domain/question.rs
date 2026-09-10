//! App-local clarification input progress built on shared session models.

pub use ag_session::{QuestionItem, default_option_index};

use crate::domain::input::InputState;

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
#[path = "question_test.rs"]
mod tests;
