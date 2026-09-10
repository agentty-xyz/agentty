//! Frontend-neutral clarification question models and selection helpers.

pub use ag_protocol::QuestionItem;

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

#[cfg(test)]
#[path = "question_test.rs"]
mod tests;
