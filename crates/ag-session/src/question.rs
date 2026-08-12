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
mod tests {
    use super::*;

    #[test]
    fn default_option_index_selects_only_predefined_options() {
        // Arrange
        let questions = [
            QuestionItem::with_options("Continue?", vec!["Yes".to_string()]),
            QuestionItem::new("Why?"),
        ];

        // Act
        let predefined = default_option_index(&questions, 0);
        let free_text = default_option_index(&questions, 1);
        let missing = default_option_index(&questions, 2);

        // Assert
        assert_eq!(predefined, Some(0));
        assert_eq!(free_text, None);
        assert_eq!(missing, None);
    }
}
