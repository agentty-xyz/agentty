use std::sync::Arc;

use ag_forge as forge;
use ag_git as git;

use super::super::ReplyEligibility;
use super::support::{
    database_with_session, session_manager_with_one_session, test_services, test_session,
};
use crate::app::SessionManager;
use crate::domain::session::Status;
use crate::domain::turn_prompt::TurnPrompt;

#[test]
/// Ensures protocol payloads without `answer` text do not update
/// titles.
fn test_parse_generated_session_title_returns_none_for_question_only_protocol_payload() {
    // Arrange
    let response_content =
        r#"{"answer":"","questions":[{"text":"Need confirmation?","options":[]}]}"#;

    // Act
    let parsed_title = SessionManager::parse_generated_session_title(response_content);

    // Assert
    assert_eq!(parsed_title, None);
}

/// Ensures lazily unloaded prompt detail cannot make a question answer
/// replace the existing session title as though it were the first prompt.
#[test]
fn test_prepare_reply_context_question_answer_keeps_existing_title() {
    // Arrange
    let session = test_session("", Status::Question, Some("Initial prompt"), "");
    let mut session_manager = session_manager_with_one_session(session);
    let prompt = TurnPrompt::from_text(
        "Clarifications:\n1. Q: Which target?\n   A: Full project".to_string(),
    );

    // Act
    let context = session_manager
        .prepare_reply_context(
            "session-id",
            &prompt,
            false,
            ReplyEligibility::QuestionAnswer,
        )
        .expect("question answer context should be available");

    // Assert
    assert_eq!(context.0, None);
    assert!(!context.1);
    assert_eq!(context.2, "session-id");
    assert_eq!(context.3, None);
    assert_eq!(session_manager.sessions()[0].prompt, "");
    assert_eq!(
        session_manager.sessions()[0].title,
        Some("Initial prompt".to_string())
    );
}

/// Ensures the structured question-answer entry point rejects stale
/// session identifiers before attempting to enqueue worker commands.
#[tokio::test]
async fn test_reply_to_question_answers_returns_false_for_missing_session() {
    // Arrange
    let session = test_session("Initial prompt", Status::Question, Some("Title"), "");
    let database = database_with_session(&session).await;
    let mut session_manager = session_manager_with_one_session(session);
    let services = test_services(
        &database,
        Arc::new(git::MockGitClient::new()),
        Arc::new(forge::MockReviewRequestClient::new()),
    );

    // Act
    let enqueued = session_manager
        .reply_to_question_answers(&services, "missing-session", "The answer")
        .await;

    // Assert
    assert!(!enqueued);
}
