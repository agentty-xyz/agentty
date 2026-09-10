use std::sync::{Arc, Mutex};

use ag_agent::MockOneShotClient;
use ag_git::MockGitClient;
use tempfile::tempdir;
use tokio::sync::mpsc;

use super::{AssistContext, FailureTracker, format_detail_lines, run_agent_assist};
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::session_message::SessionTranscript;
use crate::infra::db::AppRepositories;

#[tokio::test]
async fn test_run_agent_assist_uses_injected_one_shot_client() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let transcript = Arc::new(Mutex::new(SessionTranscript::default()));
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client
        .expect_submit()
        .times(1)
        .returning(|request| {
            assert_eq!(request.prompt, "Resolve the conflict");

            Ok(ag_agent::OneShotSubmission {
                response: ag_protocol::AgentResponse::plain("Conflict resolved"),
                stats: ag_agent::SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    diff_state: ag_agent::SessionDiffState::Unknown,
                    input_tokens: 0,
                    output_tokens: 0,
                },
            })
        });
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
    let context = AssistContext {
        app_event_tx,
        child_pid: Arc::new(Mutex::new(None)),
        db: AppRepositories::in_memory().await.expect("db should open"),
        folder: temp_directory.path().to_path_buf(),
        git_client: Arc::new(MockGitClient::new()),
        id: "session-id".to_string(),
        one_shot_client: Arc::new(one_shot_client),
        session_agent: AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5),
        session_update_versions: Arc::default(),
        transcript: Arc::clone(&transcript),
    };

    // Act
    let result = run_agent_assist(&context, "Resolve the conflict").await;

    // Assert
    result.expect("assist should succeed");
    let replay_text = transcript
        .lock()
        .expect("transcript lock should succeed")
        .replay_text();
    assert_eq!(replay_text.as_deref(), Some("Conflict resolved\n\n"));
}

#[test]
fn test_failure_tracker_observe_exceeds_after_identical_streak_limit() {
    // Arrange
    let mut tracker = FailureTracker::new(2);

    // Act
    let first_exceeded = tracker.observe("same");
    let second_exceeded = tracker.observe("same");
    let third_exceeded = tracker.observe("same");

    // Assert
    assert!(!first_exceeded);
    assert!(!second_exceeded);
    assert!(third_exceeded);
}

#[test]
fn test_failure_tracker_observe_resets_streak_for_new_fingerprint() {
    // Arrange
    let mut tracker = FailureTracker::new(2);
    let _ = tracker.observe("same");
    let _ = tracker.observe("same");

    // Act
    let exceeded = tracker.observe("other");

    // Assert
    assert!(!exceeded);
}

#[test]
fn test_failure_tracker_observe_normalizes_case_and_whitespace() {
    // Arrange
    let mut tracker = FailureTracker::new(1);

    // Act
    let first_exceeded = tracker.observe("  Same Failure  ");
    let second_exceeded = tracker.observe("same failure");

    // Assert
    assert!(!first_exceeded);
    assert!(second_exceeded);
}

#[test]
fn test_failure_tracker_observe_empty_fingerprint_resets_streak() {
    // Arrange
    let mut tracker = FailureTracker::new(1);
    let _ = tracker.observe("same");

    // Act
    let empty_exceeded = tracker.observe("  ");
    let next_exceeded = tracker.observe("same");

    // Assert
    assert!(!empty_exceeded);
    assert!(!next_exceeded);
}

#[test]
fn test_format_detail_lines_returns_bulleted_non_empty_lines() {
    // Arrange
    let detail = "line one\n\nline two";

    // Act
    let formatted = format_detail_lines(detail);

    // Assert
    assert_eq!(formatted, "- line one\n- line two");
}

#[test]
fn test_format_detail_lines_trims_lines_and_returns_empty_for_blank_detail() {
    // Arrange
    let detail = " line one \n\tline two\t";
    let blank_detail = " \n\n\t";

    // Act
    let formatted = format_detail_lines(detail);
    let blank_formatted = format_detail_lines(blank_detail);

    // Assert
    assert_eq!(formatted, "- line one\n- line two");
    assert_eq!(blank_formatted, "");
}
