use std::sync::{Arc, Mutex};

use ag_contracts::OneShotSubmission;
use ag_git::MockGitClient;
use ag_worker::MockRunClient;
use tempfile::tempdir;
use tokio::sync::mpsc;

use super::{AssistContext, FailureTracker, format_detail_lines, run_agent_assist};
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::session_message::SessionTranscript;
use crate::infra::db::AppRepositories;

#[tokio::test]
async fn test_run_agent_assist_uses_injected_run_client() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let transcript = Arc::new(Mutex::new(SessionTranscript::default()));
    let mut run_client = MockRunClient::new();
    run_client.expect_submit().times(1).returning(|request| {
        assert_eq!(request.prompt, "Resolve the conflict");

        Ok(OneShotSubmission {
            response: ag_protocol::AgentResponse::plain("Conflict resolved"),
            stats: ag_contracts::SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                diff_state: ag_contracts::SessionDiffState::Unknown,
                input_tokens: 0,
                output_tokens: 0,
            },
        })
    });
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
    let context = AssistContext {
        app_event_tx,
        available_agent_kinds: Arc::from(AgentKind::ALL),
        child_pid: Arc::new(Mutex::new(None)),
        db: AppRepositories::in_memory().await.expect("db should open"),
        folder: temp_directory.path().to_path_buf(),
        git_client: Arc::new(MockGitClient::new()),
        id: "session-id".to_string(),
        run_client: Arc::new(run_client),
        session_agent: AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet55),
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
fn test_failure_tracker_observe_normalizes_case() {
    // Arrange
    let mut tracker = FailureTracker::new(1);

    // Act
    let first_exceeded = tracker.observe("Same Failure");
    let second_exceeded = tracker.observe("same failure");

    // Assert
    assert!(!first_exceeded);
    assert!(second_exceeded);
}

#[test]
fn changing_assertion_whitespace_resets_the_identical_failure_streak() {
    // Arrange
    let mut tracker = FailureTracker::new(1);
    // Act / Assert
    for value in ["a    b", "a   b", "a  b", "a b"] {
        let diagnostic =
            format!("FAIL [1.2s] same_test\nerror: assertion failed\n  left: \"{value}\"");
        assert!(!tracker.observe(&diagnostic));
        assert!(tracker.observe(&diagnostic));
    }
}

#[test]
fn diagnostic_indentation_and_status_like_values_remain_significant() {
    // Arrange
    let mut tracker = FailureTracker::new(1);
    // Act
    let first = tracker.observe("FAIL expected a    b pid=123");
    let spacing = tracker.observe("FAIL expected a   b pid=123");
    let indentation = tracker.observe(" FAIL expected a   b pid=123");
    let trailing = tracker.observe(" FAIL expected a   b pid=123 ");
    let repeated = tracker.observe(" FAIL expected a   b pid=123 ");
    // Assert
    assert!(!first);
    assert!(!spacing);
    assert!(!indentation);
    assert!(!trailing);
    assert!(repeated);
}

#[test]
fn status_metadata_normalization_preserves_malformed_headers_and_test_identity() {
    // Arrange
    let mut tracker = FailureTracker::new(1);
    // Act / Assert
    for diagnostic in [
        "FAIL",
        "FAIL [unfinished",
        "FAIL [unknown] test",
        "FAIL [1s] test[1s]",
        "FAIL [9s] test[2s]",
    ] {
        assert!(!tracker.observe(diagnostic));
        assert!(tracker.observe(diagnostic));
    }
}

#[test]
fn internal_blank_diagnostic_lines_remain_significant_and_progress_only_output_resets() {
    // Arrange
    let mut tracker = FailureTracker::new(1);
    // Act
    let first = tracker.observe("error: assertion\nleft: a\n\nb");
    let changed = tracker.observe("error: assertion\nleft: a\n  \nb");
    let removed = tracker.observe("error: assertion\nleft: a\nb");
    let progress_only = tracker.observe("Compiling crate\n \nChecking crate");
    let after_progress = tracker.observe("error: assertion\nleft: a\nb");
    // Assert
    assert!(!first);
    assert!(!changed);
    assert!(!removed);
    assert!(!progress_only);
    assert!(!after_progress);
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
fn repeated_failure_ignores_progress_timings_and_process_ids() {
    // Arrange
    let mut tracker = FailureTracker::new(1);
    // Act
    let first = tracker.observe(
        "Compiling a v1\nNextest run ID first\nFAIL [ 1.2s ] same_test pid=123\nerror: assertion \
         [1] != [2]",
    );
    let second = tracker.observe(
        "Checking b\nFinished test build\nBlocking waiting for file lock\nNextest run ID \
         second\nFAIL [ 9.3s ] same_test pid=999\nerror: assertion [1] != [2]",
    );
    let changed = tracker.observe("FAIL [ 9.3s ] other_test pid=999\nerror: assertion [1] != [3]");
    let incomplete = tracker.observe("error: unterminated [diagnostic");
    // Assert
    assert!(!first);
    assert!(second);
    assert!(!changed);
    assert!(!incomplete);
}

#[test]
fn failure_fingerprints_preserve_assertion_durations_and_process_ids() {
    // Arrange
    let mut tracker = FailureTracker::new(1);
    // Act
    let first = tracker.observe("FAIL [1.2s] same_test\nerror: expected [1s] pid=123");
    let duration_changed = tracker.observe("FAIL [9.3s] same_test\nerror: expected [2s] pid=123");
    let process_changed = tracker.observe("FAIL [9.3s] same_test\nerror: expected [2s] pid=456");
    // Assert
    assert!(!first);
    assert!(!duration_changed);
    assert!(!process_changed);
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
