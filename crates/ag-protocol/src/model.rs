//! Structured response protocol data model and display helpers.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::question::QuestionItem;
use super::subtask::SubtaskItem;
use super::verification::VerificationVerdictItem;

/// Hard cap on the number of clarification questions extracted from one agent
/// response. Prevents runaway output from flooding the question UI even when
/// the agent ignores the prompt-level limit.
///
/// This constant is also injected into the protocol instruction prompt
/// templates so the prompt-level guidance and the server-side cap stay in
/// sync automatically.
pub(crate) const MAX_QUESTIONS: usize = 5;
/// Hard cap on the number of subtasks accepted from one orchestrator planning
/// turn. Bounds how many child sessions, worktrees, and agent CLI processes a
/// single approved plan can create even when the agent ignores the
/// prompt-level limit.
pub(crate) const MAX_SUBTASKS: usize = 8;
const QUESTIONS_FIELD_DESCRIPTION_TEMPLATE: &str =
    include_str!("template/questions_field_description.md");
const SUBTASKS_FIELD_DESCRIPTION_TEMPLATE: &str =
    include_str!("template/subtasks_field_description.md");

/// Returns the canonical JSON Schema description for the `questions` field.
///
/// This is the single source of truth for the runtime-injected schema
/// description and the matching test expectation. The static `schemars`
/// metadata on `AgentResponse::questions` is overwritten by
/// `inject_dynamic_schema_guidance` before any consumer observes the schema,
/// so all schema-facing call sites must route through this helper to stay in
/// sync.
pub(crate) fn questions_field_description() -> String {
    render_field_description_template(
        QUESTIONS_FIELD_DESCRIPTION_TEMPLATE,
        "{{ max_questions }}",
        MAX_QUESTIONS,
    )
}

/// Returns the canonical JSON Schema description for the `subtasks` field.
///
/// This mirrors [`questions_field_description`]: the static `schemars`
/// metadata on `AgentResponse::subtasks` carries only the field title, and
/// `inject_dynamic_schema_guidance` overwrites the description with this
/// helper's output before any consumer observes the schema.
pub(crate) fn subtasks_field_description() -> String {
    render_field_description_template(
        SUBTASKS_FIELD_DESCRIPTION_TEMPLATE,
        "{{ max_subtasks }}",
        MAX_SUBTASKS,
    )
}

/// Substitutes one `{{ name }}` placeholder with a runtime cap value.
///
/// The placeholder is matched after collapsing whitespace runs inside every
/// `{{ ... }}` span, because `mdformat` reflows these templates at a fixed
/// column width and will break a line in the middle of a placeholder. Matching
/// the literal text alone silently left the raw `{{ ... }}` in the description
/// shown to models, so normalization keeps the templates safe to reformat.
fn render_field_description_template(template: &str, placeholder: &str, value: usize) -> String {
    let mut rendered = String::with_capacity(template.len());
    let mut remaining = template.trim_end();

    while let Some(open_index) = remaining.find("{{") {
        let after_open = &remaining[open_index..];
        let Some(close_end) = after_open.find("}}").map(|index| index + "}}".len()) else {
            break;
        };

        rendered.push_str(&remaining[..open_index]);
        rendered.push_str(&collapse_whitespace(&after_open[..close_end]));
        remaining = &after_open[close_end..];
    }
    rendered.push_str(remaining);

    rendered.replace(placeholder, &value.to_string())
}

/// Collapses every whitespace run in `text` to one space.
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
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

/// Agent-reported disposition for one forge review thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(
    title = "ReviewCommentResolution",
    description = "Disposition reported for one targeted forge review thread."
)]
pub enum ReviewCommentResolution {
    /// The agent completed the requested action and the thread can be
    /// resolved after its reply is posted.
    Fixed,
    /// The agent determined that no code or documentation change was needed;
    /// its explanatory reply is posted while the thread remains open.
    NoChangeNeeded,
}

/// Structured outcome for one forge review thread targeted by the turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(
    title = "ReviewCommentOutcome",
    description = "Structured outcome for one forge review thread explicitly included in the turn \
                   prompt."
)]
pub struct ReviewCommentOutcome {
    /// Concise forge reply explaining what changed or why no change was needed.
    #[schemars(
        title = "reply",
        description = "Concise reply suitable for posting to the forge review thread."
    )]
    pub reply: String,
    /// Whether the thread was fixed or did not require a change.
    #[schemars(
        title = "resolution",
        description = "Whether the targeted thread was fixed or required no change."
    )]
    pub resolution: ReviewCommentResolution,
    /// Opaque forge thread identifier copied exactly from the turn prompt.
    #[schemars(
        title = "thread_id",
        description = "Opaque forge thread identifier copied exactly from the turn prompt."
    )]
    pub thread_id: String,
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
    /// Per-thread outcomes for an agent-driven forge comment-resolution turn.
    ///
    /// Ordinary session and utility turns leave this empty. Resolution
    /// workflows accept only identifiers explicitly allowlisted in the turn
    /// prompt before applying any forge-side effect.
    #[serde(default)]
    #[schemars(
        title = "review_comment_outcomes",
        description = "Per-thread outcomes for an agent-driven forge comment-resolution turn. \
                       Emit an empty array unless the prompt explicitly supplies forge thread \
                       IDs. Copy each reported `thread_id` exactly from the prompt."
    )]
    pub review_comment_outcomes: Vec<ReviewCommentOutcome>,
    /// Proposed child-session subtasks for an orchestrator planning turn.
    ///
    /// The canonical JSON Schema description is produced by
    /// [`subtasks_field_description`] and injected at schema generation time,
    /// so the static `schemars` metadata here only sets the field title.
    /// Ordinary session and utility turns leave this empty, and orchestration
    /// consumers ignore it unless the turn prompt asked for a plan.
    #[serde(default)]
    #[schemars(title = "subtasks")]
    pub subtasks: Vec<SubtaskItem>,
    /// Structured summary for session-discussion turns, or `None` for legacy
    /// payloads and one-shot prompts.
    #[serde(default)]
    #[schemars(
        title = "summary",
        description = "Structured summary for session-discussion turns, kept outside `answer` \
                       markdown. Use `null` for one-shot prompts and legacy payloads."
    )]
    pub summary: Option<AgentResponseSummary>,
    /// Per-task decisions emitted for an orchestration verification turn.
    ///
    /// Ordinary turns leave this empty. The controller must copy task keys
    /// from the coordinator envelope so only explicit passes can proceed to
    /// integration.
    #[serde(default)]
    #[schemars(
        title = "verification_verdicts",
        description = "Per-task decisions for an orchestration verification turn. Emit one item \
                       for every task in the verification envelope, and use an empty array for \
                       ordinary turns."
    )]
    pub verification_verdicts: Vec<VerificationVerdictItem>,
}

impl AgentResponse {
    /// Creates a plain response from raw text as one `answer` string.
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            answer: text.into(),
            questions: Vec::new(),
            review_comment_outcomes: Vec::new(),
            subtasks: Vec::new(),
            summary: None,
            verification_verdicts: Vec::new(),
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

    /// Returns up to [`MAX_SUBTASKS`] proposed subtasks in response order.
    ///
    /// Callers must still reject plans whose keys collide; this only bounds how
    /// much of a runaway plan is considered.
    pub fn subtask_items(&self) -> Vec<SubtaskItem> {
        self.subtasks.iter().take(MAX_SUBTASKS).cloned().collect()
    }

    /// Returns up to [`MAX_SUBTASKS`] verification decisions in response
    /// order.
    pub fn verification_verdict_items(&self) -> Vec<VerificationVerdictItem> {
        self.verification_verdicts
            .iter()
            .take(MAX_SUBTASKS)
            .cloned()
            .collect()
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
        // Arrange
        let expected_limit = format!("Emit at most {MAX_QUESTIONS} items");

        // Act
        let description = questions_field_description();
        let normalized_description = description.split_whitespace().collect::<Vec<_>>().join(" ");

        // Assert
        assert!(normalized_description.contains(&expected_limit));
        assert!(normalized_description.contains("Emit an empty array when no input is required"));
        assert!(normalized_description.contains("field defaults to an empty array when omitted"));
        assert!(normalized_description.contains("genuinely ambiguous requirement"));
        assert!(normalized_description.contains("Never request permission for agreed work"));
        assert!(normalized_description.contains("ask for satisfaction or sign-off"));
        assert!(normalized_description.contains("Execute agreed work"));
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
            review_comment_outcomes: Vec::new(),
            subtasks: Vec::new(),
            summary: None,
            verification_verdicts: Vec::new(),
        };

        // Act
        let display_text = response.to_display_text();

        // Assert
        assert_eq!(display_text, "Primary answer\n\nNeed one clarification.");
    }

    #[test]
    /// Preserves review-comment outcomes through the wire JSON contract.
    fn test_agent_response_review_comment_outcomes_round_trip() {
        // Arrange
        let response = AgentResponse {
            answer: "Addressed the comment.".to_string(),
            questions: Vec::new(),
            review_comment_outcomes: vec![ReviewCommentOutcome {
                reply: "Added the missing validation.".to_string(),
                resolution: ReviewCommentResolution::Fixed,
                thread_id: "thread-42".to_string(),
            }],
            subtasks: Vec::new(),
            verification_verdicts: Vec::new(),
            summary: None,
        };

        // Act
        let serialized = serde_json::to_string(&response).expect("response should serialize");
        let deserialized = serde_json::from_str::<AgentResponse>(&serialized)
            .expect("response should deserialize");

        // Assert
        assert_eq!(deserialized, response);
        assert!(serialized.contains(r#""resolution":"fixed""#));
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
            review_comment_outcomes: Vec::new(),
            subtasks: Vec::new(),
            verification_verdicts: Vec::new(),
            summary: None,
        };

        // Act
        let questions = response.question_items();

        // Assert
        assert_eq!(questions.len(), MAX_QUESTIONS);
    }

    #[test]
    /// Ensures subtask extraction respects the protocol subtask cap so a
    /// runaway plan cannot fan out past the bounded child-session budget.
    fn test_agent_response_subtask_items_applies_subtask_cap() {
        // Arrange
        let response = AgentResponse {
            answer: String::new(),
            questions: Vec::new(),
            review_comment_outcomes: Vec::new(),
            subtasks: (0..=MAX_SUBTASKS).map(test_subtask).collect(),
            verification_verdicts: Vec::new(),
            summary: None,
        };

        // Act
        let subtasks = response.subtask_items();

        // Assert
        assert_eq!(subtasks.len(), MAX_SUBTASKS);
        assert_eq!(subtasks[0].task_key, "task-0");
    }

    #[test]
    /// Preserves proposed subtasks through the wire JSON contract and keeps
    /// them absent from ordinary responses.
    fn test_agent_response_subtasks_round_trip() {
        // Arrange
        let response = AgentResponse {
            answer: "Proposed a plan.".to_string(),
            questions: Vec::new(),
            review_comment_outcomes: Vec::new(),
            subtasks: vec![test_subtask(1)],
            verification_verdicts: Vec::new(),
            summary: None,
        };

        // Act
        let serialized = serde_json::to_string(&response).expect("response should serialize");
        let deserialized = serde_json::from_str::<AgentResponse>(&serialized)
            .expect("response should deserialize");

        // Assert
        assert_eq!(deserialized, response);
        assert!(serialized.contains(r#""task_key":"task-1""#));
        assert_eq!(
            AgentResponse::plain("no plan").subtask_items(),
            [] as [crate::subtask::SubtaskItem; 0]
        );
    }

    #[test]
    /// Preserves typed verification decisions through JSON and applies the
    /// same bounded task count as orchestration plans.
    fn test_agent_response_verification_verdicts_round_trip_and_cap() {
        // Arrange
        let response = AgentResponse {
            answer: "Verified the settled tasks.".to_string(),
            questions: Vec::new(),
            review_comment_outcomes: Vec::new(),
            subtasks: Vec::new(),
            summary: None,
            verification_verdicts: (0..=MAX_SUBTASKS)
                .map(|index| VerificationVerdictItem {
                    reason: format!("Evidence {index}"),
                    task_key: format!("task-{index}"),
                    verdict: crate::VerificationVerdict::Pass,
                })
                .collect(),
        };

        // Act
        let serialized = serde_json::to_string(&response).expect("response should serialize");
        let deserialized = serde_json::from_str::<AgentResponse>(&serialized)
            .expect("response should deserialize");
        let verdicts = deserialized.verification_verdict_items();

        // Assert
        assert_eq!(verdicts.len(), MAX_SUBTASKS);
        assert_eq!(verdicts[0].task_key, "task-0");
        assert!(serialized.contains(r#""verdict":"pass""#));
    }

    #[test]
    /// Ensures optional `touched_areas` planning guidance defaults to an empty
    /// list instead of failing the whole turn.
    fn test_subtask_item_defaults_touched_areas() {
        // Arrange
        let raw = r#"{"prompt":"Do the work","task_key":"task-1","title":"Work"}"#;

        // Act
        let subtask =
            serde_json::from_str::<SubtaskItem>(raw).expect("subtask should parse without areas");

        // Assert
        assert_eq!(subtask.kind, crate::SubtaskKind::Implementation);
        assert_eq!(subtask.touched_areas, [] as [std::string::String; 0]);
    }

    #[test]
    /// Keeps the injected `subtasks` schema description in sync with the
    /// server-side cap the parser enforces.
    fn test_subtasks_field_description_reports_the_subtask_cap() {
        // Arrange
        let expected_limit = format!("at most {MAX_SUBTASKS} items");

        // Act
        let description = subtasks_field_description();
        let normalized_description = description.split_whitespace().collect::<Vec<_>>().join(" ");

        // Assert
        assert!(description.contains(&expected_limit));
        assert!(
            normalized_description
                .contains("Emit an empty array when no decomposition was requested")
        );
        assert!(normalized_description.contains("field defaults to an empty array when omitted"));
        assert!(normalized_description.contains("Ordinary session and utility turns"));
        assert!(normalized_description.contains("unattended in its own worktree"));
        assert!(normalized_description.contains("independently completable"));
        assert!(normalized_description.contains("without wildcards"));
        assert!(description.contains("Areas may overlap"));
        assert!(normalized_description.contains("fewer than two independent subtasks"));
        assert!(!description.contains("{{"));
    }

    #[test]
    /// Substitutes a cap placeholder that markdown reflowing wrapped across a
    /// line break, so reformatting a template cannot leak raw `{{ ... }}` text
    /// into the schema description models read.
    fn test_field_description_template_survives_a_wrapped_placeholder() {
        // Arrange
        let template = "Emit at most {{\nmax_items }} items, and no more.\n";

        // Act
        let rendered = render_field_description_template(template, "{{ max_items }}", 4);

        // Assert
        assert_eq!(rendered, "Emit at most 4 items, and no more.");
    }

    #[test]
    /// Leaves an unterminated placeholder untouched instead of truncating the
    /// remaining guidance text.
    fn test_field_description_template_keeps_unterminated_placeholder_text() {
        // Arrange
        let template = "Emit at most {{ max_items items.";

        // Act
        let rendered = render_field_description_template(template, "{{ max_items }}", 4);

        // Assert
        assert_eq!(rendered, "Emit at most {{ max_items items.");
    }

    /// Builds one deterministic subtask with a touched-area planning hint.
    fn test_subtask(index: usize) -> SubtaskItem {
        SubtaskItem {
            acceptance_criteria: vec![format!("Work item {index} is complete")],
            kind: crate::SubtaskKind::Implementation,
            prompt: format!("Complete work item {index}"),
            task_key: format!("task-{index}"),
            title: format!("Work item {index}"),
            touched_areas: vec![format!("crates/area-{index}/")],
        }
    }
}
