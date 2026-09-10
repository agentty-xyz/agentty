use std::fmt;
use std::hash::Hasher;
use std::str::FromStr;

use rustc_hash::FxHasher;

/// Durable category for one saved session transcript message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionMessageKind {
    /// Raw user prompt text without TUI prompt markers or transcript padding.
    UserPrompt,
    /// Generated agent-facing prompt retained for replay but hidden from chat.
    AgentPrompt,
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
            Self::AgentPrompt => "agent_prompt",
            Self::AssistantAnswer => "assistant_answer",
            Self::WorkflowNotice => "workflow_notice",
        }
    }

    /// Returns whether this kind represents a raw conversation message that
    /// belongs in the normal `session_message` store.
    pub fn is_conversation_message(self) -> bool {
        matches!(
            self,
            Self::UserPrompt | Self::AgentPrompt | Self::AssistantAnswer
        )
    }

    /// Returns whether this kind starts one user-visible or generated turn.
    pub fn is_prompt(self) -> bool {
        matches!(self, Self::UserPrompt | Self::AgentPrompt)
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
            "agent_prompt" => Ok(Self::AgentPrompt),
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

    /// Appends this message to a formatted transcript display buffer.
    fn append_display_text(&self, output: &mut String) {
        match self.kind {
            SessionMessageKind::UserPrompt | SessionMessageKind::AgentPrompt => {
                Self::append_user_prompt_display_text(output, &self.content);
            }
            SessionMessageKind::AssistantAnswer => {
                Self::append_assistant_answer_display_text(output, &self.content);
            }
            SessionMessageKind::WorkflowNotice => output.push_str(&self.content),
        }
    }

    /// Appends one raw user prompt using the session transcript marker and
    /// spacing.
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
            if is_clarification_prompt
                && line_index > 0
                && Self::is_clarification_question_line(prompt_line)
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

    /// Returns true for raw clarification question rows like `1. Q: Need
    /// tests?`.
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

        let content_hash = Self::content_hash_for_messages(&messages);
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
        self.content_hash = Self::content_hash_for_messages(&self.messages);
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

    /// Computes one ordered identity across message positions, kinds, and raw
    /// content so render caches can compare transcripts without rescanning them
    /// on every frame.
    fn content_hash_for_messages(messages: &[SessionMessage]) -> u64 {
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
}

/// Returns the durable message content for one kind.
///
/// User prompts preserve leading horizontal whitespace so pasted indentation
/// survives persistence while outer line breaks and trailing whitespace are
/// normalized. Assistant rows remove outer whitespace, while workflow notices
/// preserve exact content so status blocks keep their spacing.
pub fn stored_message_content(kind: SessionMessageKind, content: &str) -> String {
    match kind {
        SessionMessageKind::UserPrompt | SessionMessageKind::AgentPrompt => {
            normalized_user_prompt_content(content)
        }
        SessionMessageKind::AssistantAnswer => normalized_message_content(content),
        SessionMessageKind::WorkflowNotice => content.to_string(),
    }
}

/// Returns raw persisted message content with only outer whitespace removed.
pub fn normalized_message_content(content: &str) -> String {
    content.trim().to_string()
}

const CLARIFICATION_HEADER: &str = "Clarifications:";
const USER_PROMPT_CONTINUATION_PREFIX: &str = "   ";
const USER_PROMPT_PREFIX: &str = " › ";

/// Normalizes prompt boundaries without consuming indentation on the first
/// content line.
fn normalized_user_prompt_content(content: &str) -> String {
    content
        .trim_end()
        .trim_start_matches(['\r', '\n'])
        .to_string()
}

#[cfg(test)]
#[path = "message_test.rs"]
mod tests;
