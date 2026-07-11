use std::fmt;
use std::str::FromStr;

const CLARIFICATION_HEADER: &str = "Clarifications:";
const USER_PROMPT_CONTINUATION_PREFIX: &str = "   ";
const USER_PROMPT_PREFIX: &str = " › ";

/// Durable category for one saved session transcript message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionMessageKind {
    /// Raw user prompt text without TUI prompt markers or transcript padding.
    UserPrompt,
    /// Raw assistant answer text without transcript padding.
    AssistantAnswer,
    /// Structured summary associated with one completed agent turn.
    TurnSummary,
    /// Focused-review output associated with one completed agent turn.
    FocusedReview,
    /// Generic workflow notice emitted by Agentty session workflows.
    WorkflowNotice,
}

impl SessionMessageKind {
    /// Returns the stable database string for this message kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserPrompt => "user_prompt",
            Self::AssistantAnswer => "assistant_answer",
            Self::TurnSummary => "turn_summary",
            Self::FocusedReview => "focused_review",
            Self::WorkflowNotice => "workflow_notice",
        }
    }

    /// Returns whether this kind represents raw provider conversation history.
    pub fn is_conversation_message(self) -> bool {
        matches!(self, Self::UserPrompt | Self::AssistantAnswer)
    }
}

impl fmt::Display for SessionMessageKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for SessionMessageKind {
    type Err = SessionMessageKindParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "user_prompt" => Ok(Self::UserPrompt),
            "assistant_answer" => Ok(Self::AssistantAnswer),
            "turn_summary" => Ok(Self::TurnSummary),
            "focused_review" => Ok(Self::FocusedReview),
            "workflow_notice" => Ok(Self::WorkflowNotice),
            _ => Err(SessionMessageKindParseError {
                value: value.to_string(),
            }),
        }
    }
}

/// Error returned when a stored session message kind is unknown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionMessageKindParseError {
    value: String,
}

impl fmt::Display for SessionMessageKindParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown session message kind `{}`", self.value)
    }
}

impl std::error::Error for SessionMessageKindParseError {}

/// Lifecycle state for one stable session timeline entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionMessageState {
    /// An asynchronous producer is still preparing the final content.
    Pending,
    /// The final content completed successfully.
    Resolved,
    /// The producer completed with a user-visible failure.
    Failed,
}

impl SessionMessageState {
    /// Returns the stable database string for this state.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Resolved => "resolved",
            Self::Failed => "failed",
        }
    }
}

impl FromStr for SessionMessageState {
    type Err = SessionMessageStateParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "pending" => Ok(Self::Pending),
            "resolved" => Ok(Self::Resolved),
            "failed" => Ok(Self::Failed),
            _ => Err(SessionMessageStateParseError {
                value: value.to_string(),
            }),
        }
    }
}

/// Error returned when a stored timeline-entry state is unknown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionMessageStateParseError {
    value: String,
}

impl fmt::Display for SessionMessageStateParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown session message state `{}`", self.value)
    }
}

impl std::error::Error for SessionMessageStateParseError {}

/// One persisted transcript message for a session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionMessage {
    /// Canonical transcript text for this message.
    pub content: String,
    /// Stable producer identity used to replace pending entries in place.
    pub entry_key: Option<String>,
    /// Durable message category.
    pub kind: SessionMessageKind,
    /// Monotonic position within the owning session transcript.
    pub position: i64,
    /// Current lifecycle state for this timeline entry.
    pub state: SessionMessageState,
    /// Monotonic turn identifier used for semantic timeline ordering.
    pub turn_id: i64,
}

impl SessionMessage {
    /// Creates one transcript message at a stable transcript position.
    pub fn new(position: i64, kind: SessionMessageKind, content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            entry_key: None,
            kind,
            position,
            state: SessionMessageState::Resolved,
            turn_id: 0,
        }
    }

    /// Creates one raw user or assistant message using kind-specific storage
    /// normalization.
    pub fn conversation(position: i64, kind: SessionMessageKind, content: impl AsRef<str>) -> Self {
        Self {
            content: stored_message_content(kind, content.as_ref()),
            entry_key: None,
            kind,
            position,
            state: SessionMessageState::Resolved,
            turn_id: 0,
        }
    }

    /// Creates one turn-scoped entry with a stable replacement key.
    pub fn timeline(
        position: i64,
        turn_id: i64,
        entry_key: impl Into<String>,
        kind: SessionMessageKind,
        state: SessionMessageState,
        content: impl Into<String>,
    ) -> Self {
        Self {
            content: content.into(),
            entry_key: Some(entry_key.into()),
            kind,
            position,
            state,
            turn_id,
        }
    }
}

/// Ordered transcript view assembled from persisted session messages.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionTranscript {
    messages: Vec<SessionMessage>,
    total_content_len: usize,
}

impl SessionTranscript {
    /// Creates an ordered transcript from persisted messages.
    pub fn new(mut messages: Vec<SessionMessage>) -> Self {
        messages.sort_by_key(SessionMessage::sort_key);

        let total_content_len = messages.iter().map(|message| message.content.len()).sum();

        Self {
            messages,
            total_content_len,
        }
    }

    /// Returns whether the transcript contains no saved messages.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Returns the ordered transcript messages.
    pub fn messages(&self) -> &[SessionMessage] {
        &self.messages
    }

    /// Returns the total byte length of message content in this transcript.
    pub fn total_content_len(&self) -> usize {
        self.total_content_len
    }

    /// Appends one message to the in-memory transcript snapshot using the
    /// same content normalization as durable storage.
    pub fn append_message(&mut self, kind: SessionMessageKind, content: &str) {
        let content = stored_message_content(kind, content);
        if content.trim().is_empty() {
            return;
        }

        let position = self
            .messages
            .iter()
            .map(|message| message.position)
            .max()
            .unwrap_or(-1)
            .saturating_add(1);
        self.total_content_len = self.total_content_len.saturating_add(content.len());
        let turn_id = if kind == SessionMessageKind::UserPrompt {
            self.current_turn_id().saturating_add(1)
        } else {
            self.current_turn_id()
        };
        let mut message = SessionMessage::new(position, kind, content);
        message.turn_id = turn_id;
        self.messages.push(message);
        self.messages.sort_by_key(SessionMessage::sort_key);
    }

    /// Returns the latest turn identifier represented in the transcript.
    pub fn current_turn_id(&self) -> i64 {
        self.messages
            .iter()
            .map(|message| message.turn_id)
            .max()
            .unwrap_or_default()
    }

    /// Returns whether any timeline producer is still pending.
    pub fn has_pending_messages(&self) -> bool {
        self.messages
            .iter()
            .any(|message| message.state == SessionMessageState::Pending)
    }

    /// Inserts or replaces one stable timeline entry.
    pub fn upsert_timeline_message(&mut self, message: SessionMessage) {
        if let Some(entry_key) = message.entry_key.as_deref()
            && let Some(existing) = self
                .messages
                .iter_mut()
                .find(|existing| existing.entry_key.as_deref() == Some(entry_key))
        {
            self.total_content_len = self
                .total_content_len
                .saturating_sub(existing.content.len())
                .saturating_add(message.content.len());
            *existing = message;
        } else {
            self.total_content_len = self.total_content_len.saturating_add(message.content.len());
            self.messages.push(message);
        }
        self.messages.sort_by_key(SessionMessage::sort_key);
    }

    /// Removes one stable timeline entry and returns whether it existed.
    pub fn remove_timeline_message(&mut self, entry_key: &str) -> bool {
        let Some(message_index) = self
            .messages
            .iter()
            .position(|message| message.entry_key.as_deref() == Some(entry_key))
        else {
            return false;
        };
        let removed_message = self.messages.remove(message_index);
        self.total_content_len = self
            .total_content_len
            .saturating_sub(removed_message.content.len());

        true
    }

    /// Returns the resolved summary belonging to the latest represented turn.
    pub fn latest_turn_summary(&self) -> Option<&str> {
        let current_turn_id = self.current_turn_id();

        self.messages
            .iter()
            .rev()
            .find(|message| {
                message.turn_id == current_turn_id
                    && message.kind == SessionMessageKind::TurnSummary
                    && message.state == SessionMessageState::Resolved
            })
            .map(|message| message.content.as_str())
    }

    /// Returns formatted transcript text for replay when content exists.
    ///
    /// User and assistant rows store raw content, so replay injects the prompt
    /// marker and transcript spacing only for display and provider replay.
    pub fn replay_text(&self) -> Option<String> {
        let output = Self::display_text_for_messages(&self.messages);
        if output.trim().is_empty() {
            return None;
        }

        Some(output)
    }

    /// Returns formatted user and assistant transcript text when any
    /// conversation messages exist.
    pub fn conversation_replay_text(&self) -> Option<String> {
        let mut output = String::new();

        for message in self
            .messages
            .iter()
            .filter(|message| message.kind.is_conversation_message())
        {
            message.append_display_text(&mut output);
        }

        if output.trim().is_empty() {
            return None;
        }

        Some(output)
    }

    /// Returns provider replay text without Agentty-authored summary or
    /// focused-review metadata.
    pub fn provider_replay_text(&self) -> Option<String> {
        let mut output = String::new();

        for message in self.messages.iter().filter(|message| {
            !matches!(
                message.kind,
                SessionMessageKind::TurnSummary | SessionMessageKind::FocusedReview
            )
        }) {
            message.append_display_text(&mut output);
        }

        if output.trim().is_empty() {
            return None;
        }

        Some(output)
    }

    /// Formats one ordered message slice for session output display.
    pub(crate) fn display_text_for_messages(messages: &[SessionMessage]) -> String {
        let mut output = String::new();

        for message in messages {
            message.append_display_text(&mut output);
        }

        output
    }
}

/// Returns the durable message content for one kind.
///
/// User prompts preserve leading horizontal whitespace so pasted indentation
/// survives persistence while outer line breaks and trailing whitespace are
/// normalized. Assistant rows remove outer whitespace, while workflow notices
/// preserve exact content so status blocks keep their spacing.
pub fn stored_message_content(kind: SessionMessageKind, content: &str) -> String {
    match kind {
        SessionMessageKind::UserPrompt => normalized_user_prompt_content(content),
        SessionMessageKind::AssistantAnswer
        | SessionMessageKind::TurnSummary
        | SessionMessageKind::FocusedReview => normalized_message_content(content),
        SessionMessageKind::WorkflowNotice => content.to_string(),
    }
}

impl SessionMessage {
    /// Returns the semantic ordering key used by the visible session timeline.
    fn sort_key(&self) -> (i64, u8, i64) {
        let phase = match self.kind {
            SessionMessageKind::UserPrompt | SessionMessageKind::AssistantAnswer => 0,
            SessionMessageKind::TurnSummary => 1,
            SessionMessageKind::FocusedReview => 2,
            SessionMessageKind::WorkflowNotice => 3,
        };

        (self.turn_id, phase, self.position)
    }

    /// Appends this message to a formatted transcript display buffer.
    pub(crate) fn append_display_text(&self, output: &mut String) {
        match self.kind {
            SessionMessageKind::UserPrompt => {
                append_user_prompt_display_text(output, &self.content);
            }
            SessionMessageKind::AssistantAnswer => {
                append_assistant_answer_display_text(output, &self.content);
            }
            SessionMessageKind::TurnSummary
            | SessionMessageKind::FocusedReview
            | SessionMessageKind::WorkflowNotice => output.push_str(&self.content),
        }
    }
}

/// Returns raw persisted message content with only outer whitespace removed.
pub fn normalized_message_content(content: &str) -> String {
    content.trim().to_string()
}

/// Appends one raw user prompt using the session transcript marker and spacing.
fn append_user_prompt_display_text(output: &mut String, content: &str) {
    let content = normalized_user_prompt_content(content);
    if content.trim().is_empty() {
        return;
    }

    if !output.is_empty() {
        output.push('\n');
    }

    let is_clarification_prompt = content
        .lines()
        .next()
        .is_some_and(|line| line.trim() == CLARIFICATION_HEADER);

    for (line_index, prompt_line) in content.split('\n').enumerate() {
        if is_clarification_prompt && line_index > 0 && is_clarification_question_line(prompt_line)
        {
            output.push_str(USER_PROMPT_CONTINUATION_PREFIX);
            output.push('\n');
        }

        if line_index == 0 {
            output.push_str(USER_PROMPT_PREFIX);
        } else {
            output.push_str(USER_PROMPT_CONTINUATION_PREFIX);
        }
        output.push_str(prompt_line);
        output.push('\n');
    }
    output.push('\n');
}

/// Normalizes prompt boundaries without consuming indentation on the first
/// content line.
fn normalized_user_prompt_content(content: &str) -> String {
    content
        .trim_end()
        .trim_start_matches(['\r', '\n'])
        .to_string()
}

/// Returns true for raw clarification question rows like `1. Q: Need tests?`.
fn is_clarification_question_line(line: &str) -> bool {
    let trimmed_line = line.trim_start();
    let digit_count = trimmed_line
        .chars()
        .take_while(char::is_ascii_digit)
        .count();
    if digit_count == 0 {
        return false;
    }

    let (_, suffix) = trimmed_line.split_at(digit_count);

    suffix.starts_with(". Q: ")
}

/// Appends one raw assistant answer using session transcript spacing.
fn append_assistant_answer_display_text(output: &mut String, content: &str) {
    let content = content.trim();
    if content.is_empty() {
        return;
    }

    output.push_str(content);
    output.push_str("\n\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_message_kind_round_trips_database_value() {
        // Arrange
        let kind = SessionMessageKind::AssistantAnswer;

        // Act
        let parsed = kind
            .as_str()
            .parse::<SessionMessageKind>()
            .expect("kind should parse");

        // Assert
        assert_eq!(parsed, kind);
    }

    #[test]
    fn test_session_message_state_round_trips_database_value() {
        // Arrange
        let state = SessionMessageState::Failed;

        // Act
        let parsed = state
            .as_str()
            .parse::<SessionMessageState>()
            .expect("state should parse");

        // Assert
        assert_eq!(parsed, state);
    }

    #[test]
    fn test_session_transcript_orders_delayed_timeline_entries_before_later_turn() {
        // Arrange
        let mut first_prompt =
            SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "first prompt");
        first_prompt.turn_id = 1;
        let mut first_answer =
            SessionMessage::conversation(1, SessionMessageKind::AssistantAnswer, "first answer");
        first_answer.turn_id = 1;
        let mut second_prompt =
            SessionMessage::conversation(2, SessionMessageKind::UserPrompt, "second prompt");
        second_prompt.turn_id = 2;

        // Act
        let transcript = SessionTranscript::new(vec![
            second_prompt,
            SessionMessage::timeline(
                4,
                1,
                "focused_review:1",
                SessionMessageKind::FocusedReview,
                SessionMessageState::Resolved,
                "first review",
            ),
            first_answer,
            SessionMessage::timeline(
                3,
                1,
                "turn_summary:1",
                SessionMessageKind::TurnSummary,
                SessionMessageState::Resolved,
                "first summary",
            ),
            first_prompt,
        ]);
        let ordered_content = transcript
            .messages()
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>();

        // Assert
        assert_eq!(
            ordered_content,
            [
                "first prompt",
                "first answer",
                "first summary",
                "first review",
                "second prompt",
            ]
        );
    }

    #[test]
    fn test_session_transcript_replaces_pending_timeline_entry_in_place() {
        // Arrange
        let mut transcript = SessionTranscript::new(vec![SessionMessage::timeline(
            3,
            1,
            "branch_push:1",
            SessionMessageKind::WorkflowNotice,
            SessionMessageState::Pending,
            "Pushing branch...",
        )]);

        // Act
        transcript.upsert_timeline_message(SessionMessage::timeline(
            3,
            1,
            "branch_push:1",
            SessionMessageKind::WorkflowNotice,
            SessionMessageState::Resolved,
            "Branch pushed.",
        ));

        // Assert
        assert_eq!(transcript.messages().len(), 1);
        assert_eq!(transcript.messages()[0].position, 3);
        assert_eq!(
            transcript.messages()[0].state,
            SessionMessageState::Resolved
        );
        assert_eq!(transcript.messages()[0].content, "Branch pushed.");
    }

    #[test]
    fn test_session_transcript_formats_messages_by_position() {
        // Arrange
        let messages = vec![
            SessionMessage::conversation(2, SessionMessageKind::AssistantAnswer, " answer\n"),
            SessionMessage::conversation(1, SessionMessageKind::UserPrompt, "\nprompt "),
        ];

        // Act
        let transcript = SessionTranscript::new(messages);

        // Assert
        assert_eq!(
            transcript.replay_text().expect("expected replay text"),
            " › prompt\n\nanswer\n\n"
        );
    }

    #[test]
    fn test_session_transcript_formats_multiline_user_prompt() {
        // Arrange
        let messages = vec![SessionMessage::conversation(
            1,
            SessionMessageKind::UserPrompt,
            "first\nsecond",
        )];

        // Act
        let transcript = SessionTranscript::new(messages);

        // Assert
        assert_eq!(
            transcript.replay_text().expect("expected replay text"),
            " › first\n   second\n\n"
        );
    }

    #[test]
    fn test_session_transcript_formats_clarification_prompt_with_question_spacing() {
        // Arrange
        let messages = vec![SessionMessage::conversation(
            1,
            SessionMessageKind::UserPrompt,
            "Clarifications:\n1. Q: Need target branch?\n   A: main\n2. Q: Need tests?\n   A: yes",
        )];

        // Act
        let transcript = SessionTranscript::new(messages);

        // Assert
        assert_eq!(
            transcript.replay_text().expect("expected replay text"),
            " › Clarifications:\n   \n   1. Q: Need target branch?\n      A: main\n   \n   2. Q: \
             Need tests?\n      A: yes\n\n"
        );
    }

    #[test]
    fn test_session_transcript_formats_prompt_spacing_after_assistant_answer() {
        // Arrange
        let messages = vec![
            SessionMessage::conversation(0, SessionMessageKind::AssistantAnswer, "answer"),
            SessionMessage::conversation(1, SessionMessageKind::UserPrompt, "next prompt"),
        ];

        // Act
        let transcript = SessionTranscript::new(messages);

        // Assert
        assert_eq!(
            transcript.replay_text().expect("expected replay text"),
            "answer\n\n\n › next prompt\n\n"
        );
    }

    #[test]
    fn test_session_transcript_conversation_replay_text_excludes_workflow_notices() {
        // Arrange
        let transcript = SessionTranscript::new(vec![
            SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "review changes"),
            SessionMessage::new(
                1,
                SessionMessageKind::WorkflowNotice,
                "[Commit] No changes to commit.\n",
            ),
            SessionMessage::conversation(2, SessionMessageKind::AssistantAnswer, "done"),
        ]);

        // Act
        let conversation_text = transcript
            .conversation_replay_text()
            .expect("conversation text should render");

        // Assert
        assert_eq!(conversation_text, " › review changes\n\ndone\n\n");
        assert!(!conversation_text.contains("[Commit]"));
    }

    #[test]
    fn test_session_transcript_provider_replay_excludes_timeline_metadata() {
        // Arrange
        let transcript = SessionTranscript::new(vec![
            SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "review changes"),
            SessionMessage::timeline(
                1,
                1,
                "turn_summary:1",
                SessionMessageKind::TurnSummary,
                SessionMessageState::Resolved,
                "summary metadata",
            ),
            SessionMessage::timeline(
                2,
                1,
                "focused_review:1",
                SessionMessageKind::FocusedReview,
                SessionMessageState::Resolved,
                "review metadata",
            ),
            SessionMessage::new(
                3,
                SessionMessageKind::WorkflowNotice,
                "[Commit] No changes.\n",
            ),
        ]);

        // Act
        let provider_text = transcript
            .provider_replay_text()
            .expect("provider replay should render");

        // Assert
        assert!(provider_text.contains("review changes"));
        assert!(provider_text.contains("[Commit] No changes."));
        assert!(!provider_text.contains("summary metadata"));
        assert!(!provider_text.contains("review metadata"));
    }

    #[test]
    fn test_session_transcript_conversation_replay_text_ignores_notice_only_transcript() {
        // Arrange
        let transcript = SessionTranscript::new(vec![SessionMessage::new(
            0,
            SessionMessageKind::WorkflowNotice,
            "[Sync] Complete.\n",
        )]);

        // Act
        let conversation_text = transcript.conversation_replay_text();

        // Assert
        assert_eq!(conversation_text, None);
    }

    #[test]
    fn test_session_transcript_append_message_uses_next_position() {
        // Arrange
        let mut transcript = SessionTranscript::new(vec![SessionMessage::conversation(
            4,
            SessionMessageKind::UserPrompt,
            "prompt",
        )]);

        // Act
        transcript.append_message(SessionMessageKind::AssistantAnswer, " answer\n");

        // Assert
        assert_eq!(
            transcript.messages(),
            &[
                SessionMessage::conversation(4, SessionMessageKind::UserPrompt, "prompt"),
                SessionMessage::conversation(5, SessionMessageKind::AssistantAnswer, "answer"),
            ]
        );
    }

    #[test]
    fn test_session_transcript_total_content_len_updates_on_append() {
        // Arrange
        let mut transcript = SessionTranscript::new(vec![SessionMessage::conversation(
            4,
            SessionMessageKind::UserPrompt,
            "prompt",
        )]);

        // Act
        transcript.append_message(SessionMessageKind::AssistantAnswer, " answer\n");

        // Assert
        assert_eq!(
            transcript.total_content_len(),
            "prompt".len() + "answer".len()
        );
    }

    #[test]
    fn test_normalized_message_content_removes_outer_whitespace_only() {
        // Arrange, Act, Assert
        assert_eq!(
            normalized_message_content("\n  keep\ninner spacing  \n"),
            "keep\ninner spacing"
        );
    }

    #[test]
    fn test_stored_message_content_preserves_compatibility_spacing() {
        // Arrange
        let workflow_notice = "\n[Sync Error] failed\n";

        // Act
        let stored = stored_message_content(SessionMessageKind::WorkflowNotice, workflow_notice);

        // Assert
        assert_eq!(stored, workflow_notice);
    }

    #[test]
    fn test_stored_message_content_preserves_user_prompt_indentation() {
        // Arrange, Act, Assert
        assert_eq!(
            stored_message_content(
                SessionMessageKind::UserPrompt,
                "\n    first\n        second  \n"
            ),
            "    first\n        second"
        );
    }

    #[test]
    fn test_stored_message_content_normalizes_assistant_spacing() {
        // Arrange, Act, Assert
        assert_eq!(
            stored_message_content(SessionMessageKind::AssistantAnswer, "\n  hello  \n"),
            "hello"
        );
    }
}
