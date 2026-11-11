//! Frontend-neutral clarification question models and selection helpers.

use std::fmt::Write as _;

use ag_protocol::AgentResponse;
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

/// Returns the chat message asking one graph node's clarification questions,
/// after the answer they refer to, or `None` when every question is blank.
///
/// Storage records it with the node's response so the visible question and
/// the reply routing that depends on it commit together.
#[must_use]
pub fn node_question_notice(node_id: &str, response: &AgentResponse) -> Option<String> {
    if response
        .questions
        .iter()
        .all(|question| question.text.trim().is_empty())
    {
        return None;
    }
    let mut text = String::new();
    let answer = response.answer.trim();
    if !answer.is_empty() {
        let _ = write!(text, "{answer}\n\n");
    }
    let _ = write!(
        text,
        "`{node_id}` needs your input. Reply in chat to answer:"
    );
    push_numbered_questions(&mut text, &response.questions);

    Some(text)
}

/// Appends each question on its own numbered line, with any predefined
/// options in parentheses.
pub fn push_numbered_questions(text: &mut String, questions: &[QuestionItem]) {
    for (index, question) in questions.iter().enumerate() {
        let _ = write!(text, "\n{}. {}", index + 1, question.text);
        if !question.options.is_empty() {
            let _ = write!(text, " ({})", question.options.join(" / "));
        }
    }
}

#[cfg(test)]
#[path = "question_test.rs"]
mod tests;
