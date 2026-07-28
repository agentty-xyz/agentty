use std::fmt;
use std::hash::Hasher;
use std::str::FromStr;

use rustc_hash::FxHasher;

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
    /// Generic workflow notice emitted by Agentty session workflows.
    WorkflowNotice,
}

impl SessionMessageKind {
    /// Returns the stable database string for this message kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserPrompt => "user_prompt",
            Self::AssistantAnswer => "assistant_answer",
            Self::WorkflowNotice => "workflow_notice",
        }
    }

    /// Returns whether this kind represents a raw conversation message that
    /// belongs in the normal `session_message` store.
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

/// One persisted transcript message for a session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionMessage {
    /// Canonical transcript text for this message.
    pub content: String,
    /// Durable message category.
    pub kind: SessionMessageKind,
    /// Monotonic position within the owning session transcript.
    pub position: i64,
}

impl SessionMessage {
    /// Creates one transcript message at a stable transcript position.
    pub fn new(position: i64, kind: SessionMessageKind, content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            kind,
            position,
        }
    }

    /// Creates one raw user or assistant message using kind-specific storage
    /// normalization.
    pub fn conversation(position: i64, kind: SessionMessageKind, content: impl AsRef<str>) -> Self {
        Self {
            content: stored_message_content(kind, content.as_ref()),
            kind,
            position,
        }
    }
}

/// Ordered transcript view assembled from persisted session messages.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionTranscript {
    content_hash: u64,
    messages: Vec<SessionMessage>,
    total_content_len: usize,
}

impl SessionTranscript {
    /// Creates an ordered transcript from persisted messages.
    pub fn new(mut messages: Vec<SessionMessage>) -> Self {
        messages.sort_by_key(|message| message.position);

        let content_hash = transcript_content_hash(&messages);
        let total_content_len = messages.iter().map(|message| message.content.len()).sum();

        Self {
            content_hash,
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

    /// Returns the cached content identity for render and projection caches.
    pub fn content_hash(&self) -> u64 {
        self.content_hash
    }

    /// Returns the total byte length of message content in this transcript.
    pub fn total_content_len(&self) -> usize {
        self.total_content_len
    }

    /// Appends one message after the ordered transcript tail using the same
    /// content normalization as durable storage.
    ///
    /// [`Self::new`] sorts persisted input before this method derives the next
    /// position, so the message slice and its cached content hash retain the
    /// same ordering as a newly reconstructed transcript.
    pub fn append_message(&mut self, kind: SessionMessageKind, content: &str) {
        let content = stored_message_content(kind, content);
        if content.trim().is_empty() {
            return;
        }

        let position = self
            .messages
            .last()
            .map_or(0, |message| message.position.saturating_add(1));
        self.total_content_len = self.total_content_len.saturating_add(content.len());
        self.messages
            .push(SessionMessage::new(position, kind, content));
        self.content_hash = transcript_content_hash(&self.messages);
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

    /// Formats an ordered message slice using canonical transcript display
    /// markers and spacing.
    ///
    /// Unlike [`Self::new`], this method preserves the caller-provided order.
    /// It is useful when rendering a selected subset of an existing transcript
    /// without constructing another transcript aggregate.
    pub fn display_text_for_messages(messages: &[SessionMessage]) -> String {
        let mut output = String::new();

        for message in messages {
            message.append_display_text(&mut output);
        }

        output
    }
}

/// Computes one ordered identity across message positions, kinds, and raw
/// content so render caches can compare transcripts without rescanning them on
/// every frame.
fn transcript_content_hash(messages: &[SessionMessage]) -> u64 {
    let mut hasher = FxHasher::default();

    for message in messages {
        hasher.write_i64(message.position);
        hasher.write(message.kind.as_str().as_bytes());
        hasher.write_u8(0xff);
        hasher.write(message.content.as_bytes());
        hasher.write_u8(0xfe);
    }

    hasher.finish()
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
        SessionMessageKind::AssistantAnswer => normalized_message_content(content),
        SessionMessageKind::WorkflowNotice => content.to_string(),
    }
}

impl SessionMessage {
    /// Appends this message to a formatted transcript display buffer.
    fn append_display_text(&self, output: &mut String) {
        match self.kind {
            SessionMessageKind::UserPrompt => {
                append_user_prompt_display_text(output, &self.content);
            }
            SessionMessageKind::AssistantAnswer => {
                append_assistant_answer_display_text(output, &self.content);
            }
            SessionMessageKind::WorkflowNotice => output.push_str(&self.content),
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
        assert_eq!(kind.to_string(), "assistant_answer");
    }

    #[test]
    fn test_session_message_kind_rejects_unknown_database_value() {
        // Arrange / Act
        let error = "unknown"
            .parse::<SessionMessageKind>()
            .expect_err("unknown kind should fail");

        // Assert
        assert_eq!(error.to_string(), "unknown session message kind `unknown`");
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
    fn test_session_transcript_content_hash_tracks_exact_message_content() {
        // Arrange
        let original = SessionTranscript::new(vec![SessionMessage::conversation(
            0,
            SessionMessageKind::AssistantAnswer,
            "alpha",
        )]);
        let replacement = SessionTranscript::new(vec![SessionMessage::conversation(
            0,
            SessionMessageKind::AssistantAnswer,
            "bravo",
        )]);

        // Act
        let original_hash = original.content_hash();
        let replacement_hash = replacement.content_hash();

        // Assert
        assert_eq!(
            original.total_content_len(),
            replacement.total_content_len()
        );
        assert_ne!(original_hash, replacement_hash);
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
    fn test_session_transcript_append_message_preserves_constructor_ordering() {
        // Arrange
        let mut transcript = SessionTranscript::new(vec![
            SessionMessage::conversation(4, SessionMessageKind::AssistantAnswer, "first answer"),
            SessionMessage::conversation(1, SessionMessageKind::UserPrompt, "prompt"),
        ]);

        // Act
        transcript.append_message(SessionMessageKind::WorkflowNotice, "[Sync] Complete.\n");
        let reconstructed = SessionTranscript::new(transcript.messages().to_vec());

        // Assert
        assert_eq!(
            transcript.messages(),
            &[
                SessionMessage::conversation(1, SessionMessageKind::UserPrompt, "prompt"),
                SessionMessage::conversation(
                    4,
                    SessionMessageKind::AssistantAnswer,
                    "first answer",
                ),
                SessionMessage::new(5, SessionMessageKind::WorkflowNotice, "[Sync] Complete.\n",),
            ]
        );
        assert_eq!(transcript.content_hash(), reconstructed.content_hash());
    }

    #[test]
    fn test_session_transcript_ignores_empty_messages() {
        // Arrange
        let mut transcript = SessionTranscript::default();
        let empty_messages = [
            SessionMessage::new(0, SessionMessageKind::UserPrompt, "\n"),
            SessionMessage::new(1, SessionMessageKind::AssistantAnswer, "  "),
        ];

        // Act
        transcript.append_message(SessionMessageKind::UserPrompt, "\n");
        let replay_text = SessionTranscript::display_text_for_messages(&empty_messages);

        // Assert
        assert!(transcript.is_empty());
        assert!(replay_text.is_empty());
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
