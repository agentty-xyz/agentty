use std::fmt;
use std::str::FromStr;

/// Durable category for one saved session transcript message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionMessageKind {
    /// Generic append-only transcript fragment used only for compatibility
    /// serialization of legacy transcript text.
    TranscriptChunk,
    /// Raw user prompt text without TUI prompt markers or transcript padding.
    UserPrompt,
    /// Raw assistant answer text without transcript padding.
    AssistantAnswer,
    /// Generic workflow notice used only for compatibility serialization of
    /// legacy transcript text.
    WorkflowNotice,
    /// Exact legacy transcript copied into one message when precise parsing is
    /// not available.
    LegacyTranscript,
}

impl SessionMessageKind {
    /// Returns the stable database string for this message kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TranscriptChunk => "transcript_chunk",
            Self::UserPrompt => "user_prompt",
            Self::AssistantAnswer => "assistant_answer",
            Self::WorkflowNotice => "workflow_notice",
            Self::LegacyTranscript => "legacy_transcript",
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
            "transcript_chunk" => Ok(Self::TranscriptChunk),
            "user_prompt" => Ok(Self::UserPrompt),
            "assistant_answer" => Ok(Self::AssistantAnswer),
            "workflow_notice" => Ok(Self::WorkflowNotice),
            "legacy_transcript" => Ok(Self::LegacyTranscript),
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

    /// Creates one raw user or assistant message with outer whitespace removed.
    pub fn conversation(position: i64, kind: SessionMessageKind, content: impl AsRef<str>) -> Self {
        Self {
            content: normalized_message_content(content.as_ref()),
            kind,
            position,
        }
    }
}

/// Ordered transcript view assembled from persisted session messages.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionTranscript {
    messages: Vec<SessionMessage>,
}

impl SessionTranscript {
    /// Creates an ordered transcript from persisted messages.
    pub fn new(mut messages: Vec<SessionMessage>) -> Self {
        messages.sort_by_key(|message| message.position);

        Self { messages }
    }

    /// Returns whether the transcript contains no saved messages.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Returns the ordered transcript messages.
    pub fn messages(&self) -> &[SessionMessage] {
        &self.messages
    }

    /// Serializes the message store into the legacy `session.output`
    /// transcript.
    ///
    /// User and assistant rows store raw content, so this method injects the
    /// legacy prompt marker and transcript spacing only at the compatibility
    /// boundary. Legacy fragment kinds are appended unchanged.
    pub fn to_legacy_output(&self) -> String {
        let mut output = String::new();

        for message in &self.messages {
            message.append_legacy_output(&mut output);
        }

        output
    }
}

impl SessionMessage {
    /// Appends this raw or compatibility message to a legacy transcript buffer.
    fn append_legacy_output(&self, output: &mut String) {
        match self.kind {
            SessionMessageKind::UserPrompt => {
                append_legacy_user_prompt(output, &self.content);
            }
            SessionMessageKind::AssistantAnswer => {
                append_legacy_assistant_answer(output, &self.content);
            }
            SessionMessageKind::TranscriptChunk
            | SessionMessageKind::WorkflowNotice
            | SessionMessageKind::LegacyTranscript => output.push_str(&self.content),
        }
    }
}

/// Returns raw persisted message content with only outer whitespace removed.
pub fn normalized_message_content(content: &str) -> String {
    content.trim().to_string()
}

/// Appends one raw user prompt using the legacy transcript marker and spacing.
fn append_legacy_user_prompt(output: &mut String, content: &str) {
    let content = content.trim();
    if content.is_empty() {
        return;
    }

    if !output.is_empty() {
        output.push('\n');
    }

    for (line_index, prompt_line) in content.split('\n').enumerate() {
        if line_index == 0 {
            output.push_str(" › ");
        } else {
            output.push_str("   ");
        }
        output.push_str(prompt_line);
        output.push('\n');
    }
    output.push('\n');
}

/// Appends one raw assistant answer using legacy transcript spacing.
fn append_legacy_assistant_answer(output: &mut String, content: &str) {
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
    fn test_session_transcript_serializes_messages_by_position() {
        // Arrange
        let messages = vec![
            SessionMessage::conversation(2, SessionMessageKind::AssistantAnswer, " answer\n"),
            SessionMessage::conversation(1, SessionMessageKind::UserPrompt, "\nprompt "),
        ];

        // Act
        let transcript = SessionTranscript::new(messages);

        // Assert
        assert_eq!(transcript.to_legacy_output(), " › prompt\n\nanswer\n\n");
    }

    #[test]
    fn test_session_transcript_serializes_multiline_user_prompt() {
        // Arrange
        let messages = vec![SessionMessage::conversation(
            1,
            SessionMessageKind::UserPrompt,
            "first\nsecond",
        )];

        // Act
        let transcript = SessionTranscript::new(messages);

        // Assert
        assert_eq!(transcript.to_legacy_output(), " › first\n   second\n\n");
    }

    #[test]
    fn test_normalized_message_content_removes_outer_whitespace_only() {
        // Arrange, Act, Assert
        assert_eq!(
            normalized_message_content("\n  keep\ninner spacing  \n"),
            "keep\ninner spacing"
        );
    }
}
