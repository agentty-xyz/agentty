//! Structured response protocol data model and display helpers.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::question::QuestionItem;

/// Hard cap on the number of clarification questions extracted from one agent
/// response. Prevents runaway output from flooding the question UI even when
/// the agent ignores the prompt-level limit.
///
/// This constant is also injected into the protocol instruction prompt
/// templates so the prompt-level guidance and the server-side cap stay in
/// sync automatically.
pub(crate) const MAX_QUESTIONS: usize = 5;
const QUESTIONS_FIELD_DESCRIPTION_TEMPLATE: &str =
    include_str!("template/questions_field_description.md");

/// Returns the canonical JSON Schema description for the `questions` field.
///
/// This is the single source of truth for the runtime-injected schema
/// description and the matching test expectation. The static `schemars`
/// metadata on `AgentResponse::questions` is overwritten by
/// `inject_dynamic_schema_guidance` before any consumer observes the schema,
/// so all schema-facing call sites must route through this helper to stay in
/// sync.
pub(crate) fn questions_field_description() -> String {
    QUESTIONS_FIELD_DESCRIPTION_TEMPLATE
        .trim_end()
        .replace("{{ max_questions }}", &MAX_QUESTIONS.to_string())
}

/// Protocol-owned request family preserved across prompt submission and repair
/// retries.
///
/// Session discussion turns and isolated utility prompts share the same
/// top-level [`AgentResponse`] schema. Agentty still carries the request
/// family through transport boundaries so call sites can keep one consistent
/// protocol contract even when some callers ignore parts of the response, such
/// as the optional top-level `summary`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProtocolRequestProfile {
    /// Interactive session turn.
    SessionTurn,
    /// Isolated utility prompt.
    UtilityPrompt,
}

/// Structured session summary block emitted alongside protocol messages.
///
/// Session-discussion turns use this object instead of embedding the change
/// summary inside `answer` message text. One-shot prompts set the top-level
/// `summary` field to `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(
    title = "AgentResponseSummary",
    description = "Structured session summary block emitted alongside protocol messages instead \
                   of embedding the change summary inside `answer` markdown on session-discussion \
                   turns."
)]
pub struct AgentResponseSummary {
    /// Cumulative summary of active changes on the current session branch.
    #[schemars(
        title = "session",
        description = "Cumulative summary of active changes on the current session branch."
    )]
    pub session: String,
    /// Concise summary of only the work completed in the current turn.
    #[schemars(
        title = "turn",
        description = "Concise summary of only the work completed in the current turn."
    )]
    pub turn: String,
}

/// Wire-format protocol payload used for schema-driven provider output.
///
/// Providers that support output schemas (for example, Codex app-server) are
/// asked to emit this object as the entire assistant response payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(
    title = "AgentResponse",
    description = "Wire-format protocol payload used for schema-driven provider output. Return \
                   this object as the entire assistant response payload. Providers that support \
                   output schemas (for example, Codex app-server) are asked to emit this object \
                   directly."
)]
pub struct AgentResponse {
    /// Markdown answer text emitted for this turn.
    #[serde(default)]
    #[schemars(
        title = "answer",
        description = "Markdown answer text for delivered work, status updates, or concise \
                       completion notes. Keep clarification requests out of this field and emit \
                       them through `questions` instead."
    )]
    pub answer: String,
    /// Ordered clarification questions emitted for this turn.
    ///
    /// The canonical JSON Schema description for this field is produced by
    /// [`questions_field_description`] and injected at schema generation time
    /// by `inject_dynamic_schema_guidance`. The static `schemars` metadata
    /// here only sets the field title; the description is intentionally
    /// omitted so the helper is the single source of truth.
    #[serde(default)]
    #[schemars(title = "questions")]
    pub questions: Vec<QuestionItem>,
    /// Structured summary for session-discussion turns, or `None` for legacy
    /// payloads and one-shot prompts.
    #[serde(default)]
    #[schemars(
        title = "summary",
        description = "Structured summary for session-discussion turns, kept outside `answer` \
                       markdown. Use `null` for one-shot prompts and legacy payloads."
    )]
    pub summary: Option<AgentResponseSummary>,
}

impl AgentResponse {
    /// Creates a plain response from raw text as one `answer` string.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            answer: text.into(),
            questions: Vec::new(),
            summary: None,
        }
    }

    /// Returns display text by joining non-empty answer and question text with
    /// blank lines.
    pub fn to_display_text(&self) -> String {
        let mut display_messages = Vec::new();
        push_display_message(&mut display_messages, &self.answer);
        push_question_display_messages(&mut display_messages, &self.questions);

        display_messages.join("\n\n")
    }

    /// Returns transcript text for session output by joining non-empty
    /// `answer` content with blank lines.
    pub fn to_answer_display_text(&self) -> String {
        let mut display_messages = Vec::new();
        push_display_message(&mut display_messages, &self.answer);

        display_messages.join("\n\n")
    }

    /// Returns the answer as one single-item vector when it is non-empty.
    pub fn answers(&self) -> Vec<String> {
        let answer = self.to_answer_display_text();
        if answer.is_empty() {
            return Vec::new();
        }

        vec![answer]
    }

    /// Returns up to [`MAX_QUESTIONS`] clarification questions in response
    /// order.
    pub fn question_items(&self) -> Vec<QuestionItem> {
        self.questions.iter().take(MAX_QUESTIONS).cloned().collect()
    }
}

/// Structured response parsing failure details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentResponseParseError {
    /// Response was empty or whitespace-only.
    Empty,
    /// Response was JSON, but it did not satisfy the structured protocol
    /// contract.
    InvalidFormat {
        /// Explanation of the protocol contract violation.
        reason: String,
    },
}

impl fmt::Display for AgentResponseParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(formatter, "response is empty"),
            Self::InvalidFormat { reason } => {
                write!(formatter, "response is not valid protocol JSON: {reason}")
            }
        }
    }
}

impl std::error::Error for AgentResponseParseError {}

/// Appends one non-empty display message.
fn push_display_message(display_messages: &mut Vec<String>, text: &str) {
    if text.trim().is_empty() {
        return;
    }

    display_messages.push(text.to_string());
}

/// Appends non-empty clarification question text in order.
fn push_question_display_messages(display_messages: &mut Vec<String>, questions: &[QuestionItem]) {
    for question in questions {
        push_display_message(display_messages, &question.text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Ensures the dynamic `questions` field description renders from the
    /// checked-in prompt-schema template.
    fn test_questions_field_description_renders_template_limit() {
        // Arrange, Act
        let description = questions_field_description();
        let normalized_description = description.split_whitespace().collect::<Vec<_>>().join(" ");

        // Assert
        assert!(normalized_description.contains("Emit at most 5 items"));
        assert!(normalized_description.contains("Execute the agreed work"));
        assert!(!description.contains("{{ max_questions }}"));
    }

    #[test]
    /// Ensures display text includes the answer and clarification questions in
    /// order.
    fn test_agent_response_to_display_text_joins_answer_and_questions() {
        // Arrange
        let response = AgentResponse {
            answer: "Primary answer".to_string(),
            questions: vec![QuestionItem::new("Need one clarification.")],
            summary: None,
        };

        // Act
        let display_text = response.to_display_text();

        // Assert
        assert_eq!(display_text, "Primary answer\n\nNeed one clarification.");
    }

    #[test]
    /// Ensures question extraction respects the protocol question cap.
    fn test_agent_response_question_items_applies_question_cap() {
        // Arrange
        let response = AgentResponse {
            answer: String::new(),
            questions: (0..=MAX_QUESTIONS)
                .map(|index| QuestionItem::new(format!("Question {index}")))
                .collect(),
            summary: None,
        };

        // Act
        let questions = response.question_items();

        // Assert
        assert_eq!(questions.len(), MAX_QUESTIONS);
    }
}
