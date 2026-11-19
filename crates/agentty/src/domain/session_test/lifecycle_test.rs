//! Session lifecycle tests for drafts, cancel and fork actions, size
//! buckets, and continuation prompts.

use ag_session::{SessionId, SessionStatus as Status};

use super::super::{
    FollowUpTaskAction, SessionFollowUpTask, SessionSize, TERMINAL_CONTINUATION_PROMPT_INTRO,
};
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
fn test_awaits_first_message_only_for_draft_without_prompt() {
    // Arrange
    let fresh_session = SessionFixtureBuilder::new().status(Status::Draft).build();
    let staged_session = SessionFixtureBuilder::new()
        .status(Status::Draft)
        .prompt("Ship it")
        .build();
    let started_session = SessionFixtureBuilder::new()
        .status(Status::InProgress)
        .build();

    // Act
    let fresh_awaits = fresh_session.awaits_first_message();
    let staged_awaits = staged_session.awaits_first_message();
    let started_awaits = started_session.awaits_first_message();

    // Assert
    assert!(fresh_awaits);
    assert!(!staged_awaits);
    assert!(!started_awaits);
}

#[test]
fn test_allows_fork_action_rejects_drafts_active_and_terminal_sessions() {
    // Arrange
    let draft_review_session = SessionFixtureBuilder::new()
        .draft(true)
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

    // Act
    let allows_draft_fork = draft_review_session.allows_fork_action();
    let allows_active_fork = in_progress_session.allows_fork_action();
    let allows_done_fork = done_session.allows_fork_action();

    // Assert
    assert!(!allows_draft_fork);
    assert!(!allows_active_fork);
    assert!(!allows_done_fork);
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
    let staged_draft_session = SessionFixtureBuilder::new()
        .draft(true)
        .status(Status::Draft)
        .prompt("Ready to start")
        .build();
    let empty_draft_session = SessionFixtureBuilder::new()
        .draft(true)
        .status(Status::Draft)
        .build();

    // Act
    let can_start_staged_draft = staged_draft_session.can_start_staged_session();
    let can_start_empty_draft = empty_draft_session.can_start_staged_session();

    // Assert
    assert!(can_start_staged_draft);
    assert!(!can_start_empty_draft);
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
fn test_session_size_from_line_counts_sums_added_and_deleted_lines() {
    // Arrange
    let (added_lines, deleted_lines) = (6, 5);

    // Act
    let session_size = SessionSize::from_line_counts(added_lines, deleted_lines);

    // Assert
    assert_eq!(session_size, SessionSize::Small);
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
fn test_session_size_buckets_labels_and_parsing_cover_every_size() {
    // Arrange
    let bucket_cases = [
        (10, SessionSize::Xs),
        (30, SessionSize::Small),
        (80, SessionSize::Medium),
        (200, SessionSize::Large),
        (500, SessionSize::Xl),
        (501, SessionSize::Xxl),
    ];

    // Act
    let bucketed_sizes = bucket_cases
        .map(|(changed_line_count, _)| SessionSize::from_changed_line_count(changed_line_count));
    let labels = bucket_cases.map(|(_, size)| size.label());
    let parsed_sizes = labels.map(str::parse::<SessionSize>);
    let lowercase_sizes = ["xs", "s", "m", "l", "xl", "xxl"].map(str::parse::<SessionSize>);
    let unknown_size = "huge".parse::<SessionSize>();

    // Assert
    assert_eq!(bucketed_sizes, bucket_cases.map(|(_, size)| size));
    assert_eq!(labels, ["XS", "S", "M", "L", "XL", "XXL"]);
    assert_eq!(parsed_sizes, bucket_cases.map(|(_, size)| Ok(size)));
    assert_eq!(lowercase_sizes, bucket_cases.map(|(_, size)| Ok(size)));
    assert_eq!(unknown_size, Err("Unknown session size: huge".to_string()));
}

#[test]
fn test_follow_up_task_action_opens_launched_tasks_and_launches_others() {
    // Arrange
    let pending_task = SessionFollowUpTask {
        id: 1,
        launched_session_id: None,
        position: 0,
        text: "Add tests".to_string(),
    };
    let launched_task = SessionFollowUpTask {
        launched_session_id: Some(SessionId::from("launched-session")),
        ..pending_task.clone()
    };

    // Act
    let pending_action = pending_task.action();
    let launched_action = launched_task.action();

    // Assert
    assert_eq!(pending_action, FollowUpTaskAction::Launch);
    assert_eq!(launched_action, FollowUpTaskAction::Open);
}

#[test]
fn test_session_continuation_prompt_seed_falls_back_to_initial_prompt() {
    // Arrange
    let prompt_session = SessionFixtureBuilder::new()
        .status(Status::Done)
        .prompt("  initial prompt  ")
        .build();
    let empty_session = SessionFixtureBuilder::new()
        .status(Status::Done)
        .prompt("   ")
        .build();

    // Act
    let continuation_prompt_seed = prompt_session
        .continuation_prompt_seed()
        .expect("expected continuation prompt seed");
    let empty_prompt_seed = empty_session.continuation_prompt_seed();

    // Assert
    assert!(continuation_prompt_seed.contains("Previous session prompt:\ninitial prompt\n"));
    assert_eq!(empty_prompt_seed, None);
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
