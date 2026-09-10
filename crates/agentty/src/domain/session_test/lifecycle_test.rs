use ag_session::{SessionId, SessionRole, SessionStatus as Status};

use super::super::{SessionSize, TERMINAL_CONTINUATION_PROMPT_INTRO, can_create_stacked_child};
use super::support::test_session;
use crate::test_support::SessionFixtureBuilder;

#[test]
fn test_status_transition_draft_to_canceled() {
    // Arrange
    let current_status = Status::Draft;

    // Act
    let can_transition = current_status.can_transition_to(Status::Canceled);

    // Assert
    assert!(can_transition);
}

#[test]
fn test_allows_fork_action_rejects_drafts_children_active_and_terminal_sessions() {
    // Arrange
    let draft_review_session = SessionFixtureBuilder::new()
        .draft(true)
        .status(Status::Review)
        .build();
    let child_review_session = SessionFixtureBuilder::new()
        .draft(false)
        .parent_session_id(Some(SessionId::from("parent-session")))
        .status(Status::Review)
        .build();
    let in_progress_session = SessionFixtureBuilder::new()
        .draft(false)
        .status(Status::InProgress)
        .build();
    let done_session = SessionFixtureBuilder::new()
        .draft(false)
        .status(Status::Done)
        .build();
    let orchestrator_session = SessionFixtureBuilder::new()
        .draft(false)
        .role(SessionRole::Orchestrator)
        .status(Status::Review)
        .build();

    // Act
    let allows_draft_fork = draft_review_session.allows_fork_action();
    let allows_child_fork = child_review_session.allows_fork_action();
    let allows_active_fork = in_progress_session.allows_fork_action();
    let allows_done_fork = done_session.allows_fork_action();
    let allows_orchestrator_fork = orchestrator_session.allows_fork_action();

    // Assert
    assert!(!allows_draft_fork);
    assert!(!allows_child_fork);
    assert!(!allows_active_fork);
    assert!(!allows_done_fork);
    assert!(!allows_orchestrator_fork);
    assert!(!orchestrator_session.owns_branch_changes());
}

#[test]
fn test_can_create_stacked_child_allows_depth_five_and_rejects_depth_six() {
    // Arrange
    let root_session = SessionFixtureBuilder::new()
        .id("root")
        .draft(false)
        .status(Status::Review)
        .build();
    let level_1 = SessionFixtureBuilder::new()
        .id("level-1")
        .draft(true)
        .status(Status::Review)
        .parent_session_id(Some(SessionId::from("root")))
        .build();
    let level_2 = SessionFixtureBuilder::new()
        .id("level-2")
        .draft(true)
        .status(Status::Review)
        .parent_session_id(Some(SessionId::from("level-1")))
        .build();
    let level_3 = SessionFixtureBuilder::new()
        .id("level-3")
        .draft(true)
        .status(Status::Review)
        .parent_session_id(Some(SessionId::from("level-2")))
        .build();
    let level_4 = SessionFixtureBuilder::new()
        .id("level-4")
        .draft(true)
        .status(Status::Review)
        .parent_session_id(Some(SessionId::from("level-3")))
        .build();
    let level_5 = SessionFixtureBuilder::new()
        .id("level-5")
        .draft(true)
        .status(Status::Review)
        .parent_session_id(Some(SessionId::from("level-4")))
        .build();
    let sessions = vec![root_session, level_1, level_2, level_3, level_4, level_5];

    // Act
    let can_create_level_5 = can_create_stacked_child(&sessions, "level-4");
    let can_create_level_6 = can_create_stacked_child(&sessions, "level-5");

    // Assert
    assert!(can_create_level_5);
    assert!(!can_create_level_6);
}

#[test]
fn test_can_create_stacked_child_rejects_missing_sessions_and_invalid_parent_chains() {
    // Arrange
    let missing_parent = SessionFixtureBuilder::new()
        .id("missing-parent-child")
        .draft(true)
        .status(Status::Review)
        .parent_session_id(Some(SessionId::from("missing")))
        .build();
    let first_cycle_member = SessionFixtureBuilder::new()
        .id("cycle-a")
        .draft(true)
        .status(Status::Review)
        .parent_session_id(Some(SessionId::from("cycle-b")))
        .build();
    let second_cycle_member = SessionFixtureBuilder::new()
        .id("cycle-b")
        .draft(true)
        .status(Status::Review)
        .parent_session_id(Some(SessionId::from("cycle-a")))
        .build();
    let sessions = vec![missing_parent, first_cycle_member, second_cycle_member];

    // Act
    let missing_session_allowed = can_create_stacked_child(&sessions, "missing-session");
    let missing_parent_allowed = can_create_stacked_child(&sessions, "missing-parent-child");
    let cycle_allowed = can_create_stacked_child(&sessions, "cycle-a");

    // Assert
    assert!(!missing_session_allowed);
    assert!(!missing_parent_allowed);
    assert!(!cycle_allowed);
}

#[test]
fn test_status_from_str_draft() {
    // Arrange
    let raw_status = "Draft";

    // Act
    let status = raw_status
        .parse::<Status>()
        .expect("failed to parse status");

    // Assert
    assert_eq!(status, Status::Draft);
}

#[test]
fn test_can_start_staged_session_checks_only_draft_readiness() {
    // Arrange
    let root_draft_session = SessionFixtureBuilder::new()
        .draft(true)
        .status(Status::Draft)
        .prompt("Ready to start")
        .build();
    let stacked_draft_session = SessionFixtureBuilder::new()
        .draft(true)
        .status(Status::Draft)
        .prompt("Waiting on parent")
        .parent_session_id(Some(SessionId::from("parent-session")))
        .build();

    // Act
    let can_start_root_draft = root_draft_session.can_start_staged_session();
    let can_start_stacked_draft = stacked_draft_session.can_start_staged_session();

    // Assert
    assert!(can_start_root_draft);
    assert!(can_start_stacked_draft);
}

#[test]
fn test_status_allows_terminal_continuation_for_terminal_session_outcomes() {
    // Arrange
    let done_status = Status::Done;
    let canceled_status = Status::Canceled;
    let review_status = Status::Review;

    // Act
    let done_allows_continuation = done_status.allows_terminal_continuation();
    let canceled_allows_continuation = canceled_status.allows_terminal_continuation();
    let review_allows_continuation = review_status.allows_terminal_continuation();

    // Assert
    assert!(done_allows_continuation);
    assert!(canceled_allows_continuation);
    assert!(!review_allows_continuation);
}

#[test]
fn test_status_display_draft() {
    // Arrange
    let status = Status::Draft;

    // Act
    let displayed_status = status.to_string();

    // Assert
    assert_eq!(displayed_status, "Draft");
}

#[test]
fn test_session_allows_cancel_action_for_unstarted_draft_session() {
    // Arrange
    let mut session = test_session(None);
    session.status = Status::Draft;
    session.is_draft = true;

    // Act
    let allows_cancel_action = session.allows_cancel_action();

    // Assert
    assert!(allows_cancel_action);
}

#[test]
fn test_session_allows_cancel_action_for_draft_orchestrator() {
    // Arrange
    let mut session = test_session(None);
    session.status = Status::Draft;
    session.role = SessionRole::Orchestrator;

    // Act
    let allows_cancel_action = session.allows_cancel_action();

    // Assert
    assert!(allows_cancel_action);
}

#[test]
fn test_session_allows_cancel_action_rejects_regular_draft_session() {
    // Arrange
    let mut session = test_session(None);
    session.status = Status::Draft;

    // Act
    let allows_cancel_action = session.allows_cancel_action();

    // Assert
    assert!(!allows_cancel_action);
}

#[test]
fn test_allows_stacked_child_creation_returns_true_for_materialized_active_session() {
    // Arrange
    let session = SessionFixtureBuilder::new()
        .draft(false)
        .status(Status::Review)
        .build();

    // Act
    let allows_stacked_child = session.allows_stacked_child_creation();

    // Assert
    assert!(allows_stacked_child);
}

#[test]
fn test_stacked_child_creation_accepts_stacked_drafts_and_rejects_root_drafts_or_terminal() {
    // Arrange
    let draft_session = SessionFixtureBuilder::new()
        .draft(true)
        .status(Status::Draft)
        .build();
    let stacked_draft_session = SessionFixtureBuilder::new()
        .draft(true)
        .parent_session_id(Some(SessionId::from("parent-session")))
        .status(Status::Draft)
        .build();
    let child_session = SessionFixtureBuilder::new()
        .draft(true)
        .parent_session_id(Some(SessionId::from("parent-session")))
        .status(Status::Review)
        .build();
    let done_session = SessionFixtureBuilder::new().status(Status::Done).build();
    let merged_session = SessionFixtureBuilder::new().status(Status::Merged).build();
    let canceled_session = SessionFixtureBuilder::new()
        .status(Status::Canceled)
        .build();

    // Act
    let allows_draft_child = draft_session.allows_stacked_child_creation();
    let allows_stacked_draft_child = stacked_draft_session.allows_stacked_child_creation();
    let allows_nested_child = child_session.allows_stacked_child_creation();
    let allows_merged_child = merged_session.allows_stacked_child_creation();
    let allows_done_child = done_session.allows_stacked_child_creation();
    let allows_canceled_child = canceled_session.allows_stacked_child_creation();

    // Assert
    assert!(!allows_draft_child);
    assert!(allows_stacked_draft_child);
    assert!(allows_nested_child);
    assert!(!allows_merged_child);
    assert!(!allows_done_child);
    assert!(!allows_canceled_child);
}

#[test]
fn test_session_size_from_diff_counts_added_and_deleted_lines() {
    // Arrange
    let diff = "\
diff --git a/src/lib.rs b/src/lib.rs\n@@ -1 +1,2 @@\n-old line\n+new line\n+another line\n";

    // Act
    let session_size = SessionSize::from_diff(diff);

    // Assert
    assert_eq!(session_size, SessionSize::Xs);
}

#[test]
fn test_session_continuation_prompt_seed_uses_transcript_for_terminal_session() {
    // Arrange
    let session = SessionFixtureBuilder::new()
        .status(Status::Done)
        .project_name("project-alpha")
        .transcript("assistant transcript")
        .title(Some("Terminal session".to_string()))
        .build();

    // Act
    let continuation_prompt_seed = session
        .continuation_prompt_seed()
        .expect("expected continuation prompt seed");

    // Assert
    assert!(continuation_prompt_seed.contains(TERMINAL_CONTINUATION_PROMPT_INTRO));
    assert!(continuation_prompt_seed.contains("Previous session: Terminal session"));
    assert!(continuation_prompt_seed.contains("Project: project-alpha"));
    assert!(continuation_prompt_seed.contains("Status: Done"));
    assert!(
        continuation_prompt_seed.contains("Previous session transcript:\nassistant transcript")
    );
}

#[test]
fn test_session_continuation_prompt_seed_uses_transcript_for_canceled_session() {
    // Arrange
    let session = SessionFixtureBuilder::new()
        .status(Status::Canceled)
        .project_name("project-beta")
        .transcript("assistant transcript")
        .title(Some("Canceled session".to_string()))
        .build();

    // Act
    let continuation_prompt_seed = session
        .continuation_prompt_seed()
        .expect("expected canceled continuation prompt seed");

    // Assert
    assert!(continuation_prompt_seed.contains(TERMINAL_CONTINUATION_PROMPT_INTRO));
    assert!(continuation_prompt_seed.contains("Previous session: Canceled session"));
    assert!(continuation_prompt_seed.contains("Project: project-beta"));
    assert!(continuation_prompt_seed.contains("Status: Canceled"));
    assert!(
        continuation_prompt_seed.contains("Previous session transcript:\nassistant transcript")
    );
}

#[test]
fn test_session_continuation_prompt_seed_rejects_non_terminal_session() {
    // Arrange
    let session = SessionFixtureBuilder::new().status(Status::Review).build();

    // Act
    let continuation_prompt_seed = session.continuation_prompt_seed();

    // Assert
    assert_eq!(continuation_prompt_seed, None);
}
