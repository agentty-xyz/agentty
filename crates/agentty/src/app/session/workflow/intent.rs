//! Shared user-intent snapshots for session utility prompts.

use std::sync::{Arc, Mutex};

use ag_agent as agent;
use ag_protocol::AgentResponseSummary;

use crate::domain::session_message::{SessionMessageKind, SessionTranscript};
use crate::infra::db;

const SESSION_INTENT_HISTORY_MAX_CHARS: usize = 12_000;
const TRUNCATED_INTENT_DETAIL_MARKER: &str = "\n[... intent detail omitted ...]\n";

/// Ordered user requests captured from one session transcript.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct SessionIntentSnapshot {
    cumulative_summary: Option<String>,
    latest_user_prompt_position: Option<i64>,
    user_requests: Vec<String>,
}

impl SessionIntentSnapshot {
    /// Captures every user prompt from the shared transcript in chronological
    /// order.
    pub(super) fn from_transcript(transcript: &Arc<Mutex<SessionTranscript>>) -> Self {
        let transcript = transcript
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut latest_user_prompt_position = None;
        let mut user_requests = Vec::new();

        for message in transcript.messages() {
            if message.kind == SessionMessageKind::UserPrompt {
                latest_user_prompt_position = Some(message.position);
                user_requests.push(message.content.clone());
            }
        }

        Self {
            cumulative_summary: None,
            latest_user_prompt_position,
            user_requests,
        }
    }

    /// Captures the transcript requests and persisted cumulative session
    /// summary used to bound utility-prompt intent context.
    pub(super) async fn from_session(
        db: &db::AppRepositories,
        session_id: &str,
        transcript: &Arc<Mutex<SessionTranscript>>,
    ) -> Self {
        let mut snapshot = Self::from_transcript(transcript);
        snapshot.cumulative_summary = db
            .sessions()
            .load_session_summary(session_id)
            .await
            .ok()
            .flatten()
            .and_then(|summary| cumulative_summary_text(&summary));

        snapshot
    }

    /// Returns the cumulative summary persisted after the latest completed
    /// turn, normalized from its structured protocol payload when available.
    pub(super) fn cumulative_summary(&self) -> Option<&str> {
        self.cumulative_summary.as_deref()
    }

    /// Returns the transcript position of the newest captured user prompt.
    pub(super) fn latest_user_prompt_position(&self) -> Option<i64> {
        self.latest_user_prompt_position
    }

    /// Returns every captured user request in chronological order.
    pub(super) fn user_requests(&self) -> &[String] {
        &self.user_requests
    }
}

/// Formats bounded session-intent context inside a dynamic Markdown fence.
///
/// Short histories remain lossless. Oversized histories use the persisted
/// cumulative summary plus bounded first/latest request excerpts so title and
/// commit-message utility prompts cannot grow without limit. The fence keeps
/// arbitrary Markdown in the retained data from ending the prompt section.
pub(super) fn fenced_user_request_history(
    user_requests: &[String],
    cumulative_summary: Option<&str>,
) -> String {
    let complete_history = complete_user_request_history(user_requests);
    let history = if complete_history.chars().count() <= SESSION_INTENT_HISTORY_MAX_CHARS {
        complete_history
    } else {
        compact_user_request_history(user_requests, cumulative_summary)
    };

    let fence = agent::diff_fence(&history);

    format!("{fence}text\n{history}\n{fence}")
}

fn complete_user_request_history(user_requests: &[String]) -> String {
    let mut history = String::new();

    if user_requests.is_empty() {
        history.push_str("(no persisted user requests)");
    } else {
        for (index, user_request) in user_requests.iter().enumerate() {
            if !history.is_empty() {
                history.push('\n');
            }
            history.push_str("Request ");
            history.push_str(&(index + 1).to_string());
            history.push_str(":\n");
            history.push_str(user_request);
            history.push('\n');
        }
        history.pop();
    }

    history
}

fn compact_user_request_history(
    user_requests: &[String],
    cumulative_summary: Option<&str>,
) -> String {
    if user_requests.is_empty() {
        return complete_user_request_history(user_requests);
    }

    let cumulative_summary = cumulative_summary
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let request_count = user_requests.len();
    let summary_header = cumulative_summary.map(|_| "Cumulative session summary:\n");
    let request_header = "Compacted user request history:\n";
    let first_request_header = "Request 1:\n";
    let latest_request_header =
        (request_count > 1).then(|| format!("\nRequest {request_count}:\n"));
    let intermediate_request_count = request_count.saturating_sub(2);
    let compaction_notice = if intermediate_request_count == 0 {
        String::new()
    } else if cumulative_summary.is_some() {
        format!(
            "\n[{intermediate_request_count} intermediate request details represented by the \
             cumulative summary.]\n"
        )
    } else {
        format!(
            "\n[{intermediate_request_count} intermediate request details omitted to fit utility \
             prompt context.]\n"
        )
    };
    let fixed_character_count = summary_header.map_or(0, |header| header.len() + 2)
        + request_header.len()
        + first_request_header.len()
        + latest_request_header.as_ref().map_or(0, String::len)
        + compaction_notice.len();
    let variable_character_budget =
        SESSION_INTENT_HISTORY_MAX_CHARS.saturating_sub(fixed_character_count);
    let summary_character_budget = cumulative_summary.map_or(0, |_| variable_character_budget / 2);
    let request_character_budget = variable_character_budget - summary_character_budget;
    let first_request_character_budget = if request_count > 1 {
        request_character_budget / 2
    } else {
        request_character_budget
    };
    let latest_request_character_budget = request_character_budget - first_request_character_budget;
    let mut history = String::with_capacity(SESSION_INTENT_HISTORY_MAX_CHARS);

    if let (Some(summary_header), Some(cumulative_summary)) = (summary_header, cumulative_summary) {
        history.push_str(summary_header);
        history.push_str(&truncate_intent_text(
            cumulative_summary,
            summary_character_budget,
        ));
        history.push_str("\n\n");
    }
    history.push_str(request_header);
    history.push_str(first_request_header);
    history.push_str(&truncate_intent_text(
        &user_requests[0],
        first_request_character_budget,
    ));
    history.push_str(&compaction_notice);
    if let Some(latest_request_header) = latest_request_header {
        history.push_str(&latest_request_header);
        history.push_str(&truncate_intent_text(
            &user_requests[request_count - 1],
            latest_request_character_budget,
        ));
    }

    debug_assert!(history.chars().count() <= SESSION_INTENT_HISTORY_MAX_CHARS);

    history
}

fn truncate_intent_text(text: &str, character_budget: usize) -> String {
    let character_count = text.chars().count();
    if character_count <= character_budget {
        return text.to_string();
    }
    let marker_character_count = TRUNCATED_INTENT_DETAIL_MARKER.chars().count();
    if character_budget <= marker_character_count {
        return text.chars().take(character_budget).collect();
    }

    let retained_character_count = character_budget - marker_character_count;
    let head_character_count = retained_character_count / 2;
    let tail_character_count = retained_character_count - head_character_count;
    let head: String = text.chars().take(head_character_count).collect();
    let mut tail: Vec<char> = text.chars().rev().take(tail_character_count).collect();
    tail.reverse();

    format!(
        "{head}{TRUNCATED_INTENT_DETAIL_MARKER}{}",
        tail.into_iter().collect::<String>()
    )
}

fn cumulative_summary_text(summary: &str) -> Option<String> {
    let trimmed_summary = summary.trim();
    if trimmed_summary.is_empty() {
        return None;
    }

    let summary_text = serde_json::from_str::<AgentResponseSummary>(trimmed_summary)
        .map_or_else(|_| trimmed_summary.to_string(), |payload| payload.session);
    let summary_text = summary_text.trim();
    if summary_text.is_empty() {
        return None;
    }

    Some(summary_text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::session_message::SessionMessage;

    #[test]
    fn test_session_intent_snapshot_keeps_only_ordered_user_prompts() {
        // Arrange
        let transcript = Arc::new(Mutex::new(SessionTranscript::new(vec![
            SessionMessage::conversation(4, SessionMessageKind::UserPrompt, "Follow-up request"),
            SessionMessage::conversation(1, SessionMessageKind::UserPrompt, "Initial request"),
            SessionMessage::conversation(
                2,
                SessionMessageKind::AssistantAnswer,
                "Assistant response",
            ),
        ])));

        // Act
        let snapshot = SessionIntentSnapshot::from_transcript(&transcript);

        // Assert
        assert_eq!(
            snapshot.user_requests(),
            ["Initial request", "Follow-up request"]
        );
        assert_eq!(snapshot.latest_user_prompt_position(), Some(4));
        assert_eq!(snapshot.cumulative_summary(), None);
    }

    #[tokio::test]
    async fn test_session_intent_snapshot_loads_structured_cumulative_summary() {
        // Arrange
        let database = db::AppRepositories::in_memory().await;
        let project_id = database
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to upsert project");
        database
            .sessions()
            .insert_session("session-id", "claude-sonnet", "main", "Review", project_id)
            .await
            .expect("failed to insert session");
        database
            .sessions()
            .update_session_summary(
                "session-id",
                r#"{"session":"Preserve the complete initial intent","turn":"Add a follow-up"}"#,
            )
            .await
            .expect("failed to persist session summary");
        let transcript = Arc::new(Mutex::new(SessionTranscript::new(vec![
            SessionMessage::conversation(1, SessionMessageKind::UserPrompt, "Initial request"),
        ])));

        // Act
        let snapshot =
            SessionIntentSnapshot::from_session(&database, "session-id", &transcript).await;

        // Assert
        assert_eq!(
            snapshot.cumulative_summary(),
            Some("Preserve the complete initial intent")
        );
        assert_eq!(snapshot.user_requests(), ["Initial request"]);
    }

    #[test]
    fn test_fenced_user_request_history_handles_empty_and_embedded_fences() {
        // Arrange
        let user_requests = vec![
            "Initial request".to_string(),
            "Follow-up with ```text\ncontent\n```".to_string(),
        ];

        // Act
        let fenced_history = fenced_user_request_history(&user_requests, None);
        let empty_history = fenced_user_request_history(&[], None);

        // Assert
        assert!(fenced_history.starts_with("````text\nRequest 1:\nInitial request"));
        assert!(fenced_history.contains("Request 2:\nFollow-up with ```text"));
        assert!(fenced_history.ends_with("\n````"));
        assert_eq!(empty_history, "```text\n(no persisted user requests)\n```");
    }

    #[test]
    fn test_fenced_user_request_history_compacts_oversized_intent_context() {
        // Arrange
        let user_requests = vec![
            format!(
                "Initial intent {}",
                "a".repeat(SESSION_INTENT_HISTORY_MAX_CHARS)
            ),
            "Add the middle-session requirement".to_string(),
            format!(
                "Latest refinement {}",
                "z".repeat(SESSION_INTENT_HISTORY_MAX_CHARS)
            ),
        ];
        let cumulative_summary = "Preserve the initial intent and middle-session requirement";

        // Act
        let fenced_history = fenced_user_request_history(&user_requests, Some(cumulative_summary));

        // Assert
        assert!(fenced_history.contains("Cumulative session summary:\n"));
        assert!(fenced_history.contains(cumulative_summary));
        assert!(fenced_history.contains("Request 1:\nInitial intent"));
        assert!(fenced_history.contains("1 intermediate request details represented"));
        assert!(fenced_history.contains("Request 3:\nLatest refinement"));
        assert!(fenced_history.contains(TRUNCATED_INTENT_DETAIL_MARKER));
        assert!(
            fenced_history.chars().count() <= SESSION_INTENT_HISTORY_MAX_CHARS * 3 + 8,
            "dynamic fences and bounded content must have a finite worst-case size"
        );
    }

    #[test]
    fn test_fenced_user_request_history_compacts_one_oversized_request() {
        // Arrange
        let user_requests = vec!["a".repeat(SESSION_INTENT_HISTORY_MAX_CHARS + 1)];

        // Act
        let fenced_history = fenced_user_request_history(&user_requests, None);

        // Assert
        assert!(fenced_history.contains("Compacted user request history:\nRequest 1:\n"));
        assert!(!fenced_history.contains("Request 2:"));
        assert!(fenced_history.contains(TRUNCATED_INTENT_DETAIL_MARKER));
    }

    #[test]
    fn test_fenced_user_request_history_marks_unsummarized_omissions() {
        // Arrange
        let user_requests = vec![
            "a".repeat(SESSION_INTENT_HISTORY_MAX_CHARS),
            "Middle request".to_string(),
            "Latest request".to_string(),
        ];

        // Act
        let fenced_history = fenced_user_request_history(&user_requests, None);

        // Assert
        assert!(
            fenced_history
                .contains("1 intermediate request details omitted to fit utility prompt context")
        );
    }

    #[test]
    fn test_truncate_intent_text_honors_a_marker_sized_budget() {
        // Arrange
        let character_budget = TRUNCATED_INTENT_DETAIL_MARKER.chars().count();

        // Act
        let truncated_text = truncate_intent_text(
            &"a".repeat(character_budget.saturating_add(1)),
            character_budget,
        );

        // Assert
        assert_eq!(truncated_text.chars().count(), character_budget);
    }

    #[test]
    fn test_compact_user_request_history_handles_empty_requests() {
        // Arrange, Act
        let compacted_history = compact_user_request_history(&[], Some("Existing summary"));

        // Assert
        assert_eq!(compacted_history, "(no persisted user requests)");
    }

    #[test]
    fn test_cumulative_summary_text_handles_plain_and_empty_payloads() {
        // Arrange, Act
        let plain_summary = cumulative_summary_text("  Keep the original intent  ");
        let empty_structured_summary = cumulative_summary_text(r#"{"session":" ","turn":"Done"}"#);
        let empty_summary = cumulative_summary_text("   ");

        // Assert
        assert_eq!(plain_summary.as_deref(), Some("Keep the original intent"));
        assert_eq!(empty_structured_summary, None);
        assert_eq!(empty_summary, None);
    }
}
