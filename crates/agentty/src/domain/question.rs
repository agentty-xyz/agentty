//! App-local clarification input progress built on shared session models.

use ag_session::QuestionAnswer;
pub use ag_session::{QuestionItem, default_option_index};

use crate::domain::composer::{PromptAttachment, PromptAttachmentState, drain_prompt_submission};
use crate::domain::input::InputState;

/// Formats one answered question set as the clarification reply sent to the
/// agent that asked, whether a session turn or graph work.
#[must_use]
pub fn question_answer_message(answers: &[QuestionAnswer]) -> String {
    let mut lines = vec!["Clarifications:".to_string()];

    for (question_index, answer) in answers.iter().enumerate() {
        lines.push(format!("{}. Q: {}", question_index + 1, answer.question));
        lines.push(format!("   A: {}", answer.answer));
    }

    lines.join("\n")
}

/// Partially answered clarification state saved when the user leaves question
/// mode for the sessions list.
///
/// Stored per session so already-submitted answers survive exiting with `q`
/// and are restored the next time the session's question mode opens.
pub struct QuestionProgress {
    /// Pasted images owned by submitted answers and the current input.
    pub attachment_state: QuestionAttachmentState,
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

/// Pasted images owned by one clarification answer set.
///
/// `[Image #n]` numbering continues across the set, so the combined
/// clarification reply names every image exactly once.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QuestionAttachmentState {
    /// Images referenced by answers already submitted for earlier questions.
    pub answered: Vec<PromptAttachment>,
    /// Placeholder tracking for the answer currently being edited.
    pub composer: PromptAttachmentState,
    /// Position in `answered` where the most recently finished answer's
    /// images begin, so reopening that answer never reclaims images owned by
    /// earlier answers that reuse the same placeholder text.
    last_answer_start: usize,
}

impl QuestionAttachmentState {
    /// Drains the free-text answer in `input`, keeping the images its text
    /// still references for the clarification reply.
    ///
    /// Returns the answer text and the deleted images that undo can no longer
    /// restore, which the caller must clean up.
    pub fn take_answer(&mut self, input: &mut InputState) -> (String, Vec<PromptAttachment>) {
        let released_attachments = std::mem::take(&mut self.composer.archived_attachments);
        let submission = drain_prompt_submission(&mut self.composer, input);
        self.last_answer_start = self.answered.len();
        self.answered.extend(submission.attachments);
        self.continue_numbering();

        (submission.text, released_attachments)
    }

    /// Releases the current answer's images when a predefined option answers
    /// the question instead, returning them for cleanup.
    pub fn discard_answer(&mut self) -> Vec<PromptAttachment> {
        let released_attachments = self
            .composer
            .attachments
            .drain(..)
            .chain(self.composer.archived_attachments.drain(..))
            .collect();
        self.composer.reset();
        self.last_answer_start = self.answered.len();
        self.continue_numbering();

        released_attachments
    }

    /// Moves the images referenced by a restored final answer in `input` back
    /// into the composer so the answer can be edited and sent again.
    ///
    /// Only the final answer's own images are reclaimed; earlier answers keep
    /// theirs even when the final text repeats one of their placeholders.
    pub fn reopen_answer(&mut self, input: &InputState) {
        let last_answer_start = self.last_answer_start.min(self.answered.len());
        let last_answer_attachments = self.answered.split_off(last_answer_start);
        let missing_attachments = self
            .composer
            .restore_drained(input, last_answer_attachments);
        self.answered.extend(missing_attachments);
        self.last_answer_start = self.answered.len();
        self.continue_numbering();
    }

    /// Returns every image the answer set owns, for cleanup.
    #[must_use]
    pub fn into_attachments(self) -> Vec<PromptAttachment> {
        self.answered
            .into_iter()
            .chain(self.composer.attachments)
            .chain(self.composer.archived_attachments)
            .collect()
    }

    /// Numbers the next pasted image after every image the set still owns.
    fn continue_numbering(&mut self) {
        self.composer.refresh_next_attachment_number();
        let next_after_answered = self
            .answered
            .iter()
            .map(|attachment| attachment.attachment_number)
            .max()
            .unwrap_or_default()
            .saturating_add(1);
        self.composer.next_attachment_number = self
            .composer
            .next_attachment_number
            .max(next_after_answered);
    }
}

#[cfg(test)]
#[path = "question_test.rs"]
mod tests;
