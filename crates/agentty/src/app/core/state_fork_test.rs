use std::sync::Arc;

use super::App;
use crate::app::AppError;
use crate::domain::session::{SessionDiffState, SessionId, SessionStats, Status};
use crate::domain::session_message::SessionMessageKind;
use crate::infra::tmux::MockTmuxClient;
use crate::presentation::app_mode::AppMode;

/// Prompt text copied through fork snapshot tests.
const FORK_SOURCE_PROMPT: &str = "Build the fork workflow";
/// Assistant text copied through fork snapshot tests.
const FORK_SOURCE_ANSWER: &str = "Fork workflow complete";

/// Creates a real git-backed source session and marks it review-ready for
/// fork tests.
async fn create_review_source_session_for_fork_test(app: &mut App) -> String {
    let source_session_id = app
        .create_session()
        .await
        .expect("failed to create source session");
    let source_status = Status::Review.to_string();
    app.services
        .db()
        .sessions()
        .update_session_status_with_timing_at(&source_session_id, &source_status, 0)
        .await
        .expect("failed to mark source as review");

    persist_fork_source_runtime_linkage(app, &source_session_id).await;
    persist_fork_source_transcript(app, &source_session_id).await;
    let source_folder = app
        .sessions
        .session_for_id(&source_session_id)
        .expect("missing source session")
        .folder
        .clone();
    std::fs::write(source_folder.join("README.md"), "dirty source worktree")
        .expect("failed to modify source worktree");
    crate::test_support::set_session_status_for_test(app, &source_session_id, Status::Review);

    source_session_id
}

/// Persists source-only linkage that a fork must intentionally clear.
async fn persist_fork_source_runtime_linkage(app: &App, source_session_id: &str) {
    app.services
        .db()
        .sessions()
        .update_session_provider_conversation_id(
            source_session_id,
            Some("provider-thread".to_string()),
        )
        .await
        .expect("failed to persist provider conversation id");
    app.services
        .db()
        .sessions()
        .update_session_instruction_conversation_id(
            source_session_id,
            Some("instruction-thread".to_string()),
        )
        .await
        .expect("failed to persist instruction conversation id");
    app.services
        .db()
        .sessions()
        .update_session_published_upstream_ref(
            source_session_id,
            Some("origin/wt/source".to_string()),
        )
        .await
        .expect("failed to persist upstream ref");
    app.services
        .db()
        .sessions()
        .update_session_stats(
            source_session_id,
            &SessionStats {
                input_tokens: 13,
                output_tokens: 21,
                ..SessionStats::default()
            },
        )
        .await
        .expect("failed to persist source usage stats");
    app.services
        .db()
        .sessions()
        .update_session_diff_stats(1, 0, true, source_session_id, "XS")
        .await
        .expect("failed to persist source diff stats");
}

/// Persists source transcript rows that a fork must copy.
async fn persist_fork_source_transcript(app: &App, source_session_id: &str) {
    app.services
        .db()
        .sessions()
        .append_session_message(
            source_session_id,
            SessionMessageKind::UserPrompt,
            FORK_SOURCE_PROMPT,
        )
        .await
        .expect("failed to append source user prompt");
    app.services
        .db()
        .sessions()
        .append_session_message(
            source_session_id,
            SessionMessageKind::AssistantAnswer,
            FORK_SOURCE_ANSWER,
        )
        .await
        .expect("failed to append source assistant answer");
}

/// Asserts that a fork is open and contains the copied transcript without
/// source runtime linkage.
async fn assert_forked_session_snapshot(app: &App, forked_session_id: &str) {
    assert!(matches!(
        app.mode,
        AppMode::View {
            ref session_id,
            ..
        } if session_id.as_str() == forked_session_id
    ));
    assert!(matches!(
        app.selected_session(),
        Some(session) if session.id == forked_session_id
            && session.status == Status::Review
            && session.parent_session_id.is_none()
            && session.published_upstream_ref.is_none()
            && session.stats.added_lines == 0
            && session.stats.deleted_lines == 0
            && session.stats.diff_state == SessionDiffState::Unknown
            && session.stats.input_tokens == 0
            && session.stats.output_tokens == 0
    ));
    let forked_session = app
        .sessions
        .session_for_id(forked_session_id)
        .expect("missing forked session");
    assert_eq!(
        std::fs::read_to_string(forked_session.folder.join("README.md"))
            .expect("failed to read forked worktree"),
        "test"
    );

    let forked_messages = app
        .services
        .db()
        .sessions()
        .load_session_messages(forked_session_id)
        .await
        .expect("failed to load forked messages");
    assert_eq!(forked_messages.len(), 2);
    assert_eq!(forked_messages[0].kind, "user_prompt");
    assert_eq!(forked_messages[0].content, FORK_SOURCE_PROMPT);
    assert_eq!(forked_messages[1].kind, "assistant_answer");
    assert_eq!(forked_messages[1].content, FORK_SOURCE_ANSWER);
    assert_eq!(
        app.services
            .db()
            .sessions()
            .get_session_provider_conversation_id(forked_session_id)
            .await
            .expect("failed to load fork provider conversation id"),
        None
    );
    assert_eq!(
        app.services
            .db()
            .sessions()
            .get_session_instruction_conversation_id(forked_session_id)
            .await
            .expect("failed to load fork instruction conversation id"),
        None
    );
}

#[tokio::test]
async fn test_fork_session_from_dirty_source_prepares_frozen_snapshot() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let source_session_id = create_review_source_session_for_fork_test(&mut app).await;

    // Act
    let forked_session_id = app
        .fork_session(&source_session_id)
        .await
        .expect("expected session fork to succeed");

    crate::test_support::finish_session_creation_tasks(&mut app).await;

    // Assert
    assert_ne!(forked_session_id, source_session_id);
    assert_forked_session_snapshot(&app, &forked_session_id).await;
}

#[tokio::test]
async fn test_fork_session_rejects_non_review_source_session() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let source_session_id = app
        .create_session()
        .await
        .expect("failed to create source session");

    // Act
    let result = app.fork_session(&source_session_id).await;

    // Assert
    assert!(matches!(
        result,
        Err(AppError::Session(crate::app::SessionError::Workflow(message)))
            if message == "Only root review-ready sessions can be forked"
    ));
}

#[tokio::test]
async fn test_fork_session_rejects_stacked_child_source_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    let child_session = crate::test_support::SessionFixtureBuilder::new()
        .id("child-source")
        .parent_session_id(Some(SessionId::from("parent-session")))
        .status(Status::Review)
        .build();
    app.sessions.push_session(child_session);

    // Act
    let result = app.fork_session("child-source").await;

    // Assert
    assert!(matches!(
        result,
        Err(AppError::Session(crate::app::SessionError::Workflow(message)))
            if message == "Only root review-ready sessions can be forked"
    ));
}
