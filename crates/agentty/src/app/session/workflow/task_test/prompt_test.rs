use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_agent as agent;
use ag_agent::MockOneShotClient;
use ag_git::MockGitClient;
use tokio::sync::mpsc;

use super::super::{
    AutoCommitOutcome, SESSION_COMMIT_COAUTHORED_BY_AGENTTY_TRAILER, SessionTaskService,
    is_input_size_error,
};
use super::support::insert_review_session;
use crate::app::AppEvent;
use crate::app::assist::AssistContext;
use crate::app::session::SessionError;
use crate::db::AppRepositories;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::session_message::SessionTranscript;

#[tokio::test]
/// Verifies size failures preserve pending changes and never enter repair.
async fn test_handle_auto_commit_stops_on_input_size() {
    // Arrange
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .returning(|_| Box::pin(async { Ok(false) }));
    mock_git_client
        .expect_diff()
        .times(1)
        .returning(|_, _| Box::pin(async { Ok("+pending change".to_string()) }));
    mock_git_client
        .expect_has_commits_since()
        .times(1)
        .returning(|_, _| Box::pin(async { Ok(false) }));
    mock_git_client
        .expect_commit_all_preserving_single_commit()
        .never();
    mock_git_client
        .expect_diff_changed_files()
        .times(1)
        .returning(|_, _| Box::pin(async { Ok(vec!["pending.rs".to_string()]) }));
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client
        .expect_submit()
        .times(2)
        .returning(|request| {
            assert!(
                request
                    .prompt
                    .contains("Generate the canonical session commit message")
            );

            Err(agent::OneShotError::new(
                "Input exceeds the maximum length of 1048576 characters.",
            ))
        });
    let database = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&database, AgentModel::Gpt56Sol.as_str()).await;
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let transcript = Arc::new(Mutex::new(SessionTranscript::default()));
    let context = AssistContext {
        app_event_tx,
        child_pid: Arc::new(Mutex::new(None)),
        db: database.clone(),
        folder: PathBuf::from("project"),
        git_client: Arc::new(mock_git_client),
        id: "session-id".to_string(),
        one_shot_client: Arc::new(one_shot_client),
        session_agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
        session_update_versions: Arc::default(),
        transcript: Arc::clone(&transcript),
    };

    // Act
    let outcome = SessionTaskService::handle_auto_commit(context).await;

    // Assert
    assert!(matches!(outcome, AutoCommitOutcome::Failed));
    let messages = database
        .sessions()
        .load_session_messages("session-id")
        .await
        .expect("persisted messages should load");
    assert_eq!(messages.len(), 1);
    let message = &messages[0].content;
    assert!(message.contains("[Commit Error] Input exceeds the maximum length"));
    assert!(!message.contains("Commit Assist"));
    let events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(events.contains(&AppEvent::SessionProgressUpdated {
        progress_message: None,
        session_id: "session-id".into(),
    }));
}

#[test]
/// Verifies context-window overflow error detection recognizes provider
/// diagnostics.
fn test_is_input_size_error_detects_window_limits() {
    // Arrange
    let overflow_error = SessionError::OneShot(agent::OneShotError::new(
        "Codex app-server failed: contextWindowExceeded",
    ));
    let other_error = SessionError::Workflow("network timeout".to_string());

    // Act
    let overflow_is_detected = is_input_size_error(&overflow_error);
    let other_is_detected = is_input_size_error(&other_error);

    // Assert
    assert!(overflow_is_detected);
    assert!(!other_is_detected);
}

#[test]
/// Verifies session commit-message prompts include continuity, the
/// cumulative diff, and current skill-directory guidance.
fn test_session_commit_message_prompt_includes_continuity_and_diff() {
    // Arrange
    let diff = "diff --git a/a.rs b/a.rs";
    let current_commit_message = Some("Keep session commit accurate");

    // Act
    let prompt =
        SessionTaskService::session_commit_message_prompt(diff, current_commit_message, false)
            .expect("prompt should render");

    // Assert
    assert!(prompt.contains("Keep session commit accurate"));
    assert!(prompt.contains(diff));
    assert!(prompt.contains("required protocol JSON object"));
    assert!(prompt.contains("Apply this precedence order"));
    assert!(prompt.contains("Explicit user instructions in the diff request"));
    assert!(prompt.contains("most specific applicable repository guidance"));
    assert!(prompt.contains("`.agents/skills/`"));
    assert!(prompt.contains("present simple tense"));
    assert!(prompt.contains("Conventional Commit prefixes"));
    assert!(prompt.contains("refine that same message"));
    assert!(prompt.contains("Do not invent unsupported changes"));
    assert!(!prompt.contains("`.gemini/skills/`"));
    assert!(!prompt.contains("Return one plain-text commit message"));
    assert!(!prompt.contains(SESSION_COMMIT_COAUTHORED_BY_AGENTTY_TRAILER));
    let fenced_diff = format!("```diff\n{diff}\n```");
    assert!(
        prompt.contains(&fenced_diff),
        "commit-message prompt must wrap the diff in a ```diff``` fence so `@`-prefixed decorator \
         tokens are not misread as file mentions"
    );
}

#[test]
/// Ensures the commit-message prompt escapes a triple-backtick fence that
/// appears inside the diff itself (for example when committing changes to
/// a Markdown or prompt-template file) by widening the outer fence so it
/// cannot be terminated by the diff content.
fn test_session_commit_message_prompt_escapes_triple_backtick_fence_in_diff() {
    // Arrange
    let diff = concat!(
        "diff --git a/a.md b/a.md\n",
        "+```\n",
        "+example fenced block\n",
        "+```\n",
    );
    let current_commit_message: Option<&str> = None;

    // Act
    let prompt =
        SessionTaskService::session_commit_message_prompt(diff, current_commit_message, false)
            .expect("prompt should render");

    // Assert
    assert!(
        prompt.contains("````diff\n"),
        "outer fence must be longer than the longest backtick run in the diff to preserve prompt \
         boundaries"
    );
    let matches = prompt.matches("\n````").count();
    assert!(
        matches >= 2,
        "prompt must contain an opening and closing 4-backtick fence, got {matches} occurrences"
    );
    assert!(prompt.contains("+```\n"));
}

#[test]
/// Verifies prompt rendering strips the Agentty trailer from existing
/// commit-message continuity before sending it back to the model.
fn test_session_commit_message_prompt_strips_coauthor_trailer_from_continuity() {
    // Arrange
    let diff = "diff --git a/a.rs b/a.rs";
    let current_commit_message =
        format!("Keep session commit accurate\n\n{SESSION_COMMIT_COAUTHORED_BY_AGENTTY_TRAILER}");

    // Act
    let prompt = SessionTaskService::session_commit_message_prompt(
        diff,
        Some(current_commit_message.as_str()),
        false,
    )
    .expect("prompt should render");

    // Assert
    assert!(!prompt.contains(SESSION_COMMIT_COAUTHORED_BY_AGENTTY_TRAILER));
    assert!(prompt.contains("Keep session commit accurate"));
}

#[test]
/// Ensures commit assistance prompts include the raw git failure details.
fn test_auto_commit_assist_prompt_includes_commit_error() {
    // Arrange
    let commit_error = "Failed to commit: merge conflict remains";

    // Act
    let prompt = SessionTaskService::auto_commit_assist_prompt(commit_error)
        .expect("auto commit assist prompt should render");

    // Assert
    assert!(prompt.contains("Failed to commit: merge conflict remains"));
    assert!(prompt.contains("only the minimal edits needed"));
    assert!(prompt.contains("intended behavior"));
    assert!(prompt.contains("limited to read-only commands"));
    assert!(prompt.contains("Never run mutating git commands or create commits"));
    assert!(prompt.contains("return the required protocol JSON object"));
    assert!(prompt.contains("summarize the fix in\n  `answer`"));
    assert!(prompt.contains("leave `questions` empty"));
}
