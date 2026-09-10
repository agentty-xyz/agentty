use std::collections::HashMap;
use std::path::PathBuf;

use ag_agent::{SessionDiffState, SessionStats, SpeedMode};
use ag_session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary, SessionId, SessionRole,
    SessionStatus as Status, activity_day_key_with_offset,
};

use super::super::super::agent::AgentSelection;
use super::super::{
    Session, SessionDiffStats, SessionSize, can_append_session_to_stack,
    can_mutate_session_branch_in_stack, can_reply_to_session_in_stack,
    can_start_staged_session_in_stack, has_reserved_branch_work_in_stack, session_is_descendant_of,
};
use super::support::test_session;
use crate::domain::agent::AgentModel;
use crate::domain::transient_message::TransientMessageStore;
use crate::test_support::SessionFixtureBuilder;

#[test]
fn test_in_progress_duration_seconds_accumulates_closed_and_open_intervals() {
    // Arrange
    let session = Session {
        base_branch: "main".to_string(),
        created_at: 0,
        draft_attachments: Vec::new(),
        folder: PathBuf::new(),
        follow_up_tasks: Vec::new(),
        id: "session-id".into(),
        in_progress_started_at: Some(200),
        in_progress_total_seconds: 90,
        is_draft: false,
        controller_session_id: None,
        orchestration_progress: None,
        role: SessionRole::default(),
        agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
        parent_session_id: None,
        permission_mode: crate::domain::permission::PermissionMode::AutoEdit,
        personality_id: None,
        project_name: "project".to_string(),
        prompt: String::new(),
        queued_messages: Vec::new(),
        reasoning_level_override: None,
        response_style: crate::domain::agent::ResponseStyle::default(),
        published_upstream_ref: None,
        questions: Vec::new(),
        review_request: None,
        size: SessionSize::Xs,
        speed_mode: SpeedMode::default(),
        stats: SessionStats::default(),
        status: Status::InProgress,
        title: None,
        transcript: None,
        updated_at: 0,
        transient_messages: TransientMessageStore::default(),
    };

    // Act
    let duration_seconds = session.in_progress_duration_seconds(260);

    // Assert
    assert_eq!(duration_seconds, 150);
}

// -- forge_indicator tests -----------------------------------------------

#[test]
fn test_forge_indicator_returns_open_symbol_with_display_id() {
    // Arrange
    let mut session = test_session(None);
    session.review_request = Some(ReviewRequest {
        last_refreshed_at: 0,
        summary: ReviewRequestSummary {
            display_id: "#42".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "wt/session-id".to_string(),
            state: ReviewRequestState::Open,
            status_summary: None,
            target_branch: "main".to_string(),
            title: "feat".to_string(),
            web_url: String::new(),
        },
    });

    // Act
    let indicator = session.forge_indicator();

    // Assert
    assert_eq!(indicator, "⊙ #42");
}

#[test]
fn test_forge_indicator_returns_closed_symbol_with_display_id() {
    // Arrange
    let mut session = test_session(None);
    session.review_request = Some(ReviewRequest {
        last_refreshed_at: 0,
        summary: ReviewRequestSummary {
            display_id: "#7".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "wt/session-id".to_string(),
            state: ReviewRequestState::Closed,
            status_summary: None,
            target_branch: "main".to_string(),
            title: "feat".to_string(),
            web_url: String::new(),
        },
    });

    // Act
    let indicator = session.forge_indicator();

    // Assert
    assert_eq!(indicator, "✗ #7");
}

#[test]
fn test_forge_indicator_returns_empty_when_no_forge_context() {
    // Arrange
    let session = test_session(None);

    // Act
    let indicator = session.forge_indicator();

    // Assert
    assert_eq!(indicator, "");
}

#[test]
fn test_session_is_descendant_of_walks_nested_chain_and_rejects_missing_parent() {
    // Arrange
    let root_session = SessionFixtureBuilder::new().id("root-session").build();
    let parent_session = SessionFixtureBuilder::new()
        .id("parent-session")
        .parent_session_id(Some(SessionId::from("root-session")))
        .build();
    let child_session = SessionFixtureBuilder::new()
        .id("child-session")
        .parent_session_id(Some(SessionId::from("parent-session")))
        .build();
    let orphan_session = SessionFixtureBuilder::new()
        .id("orphan-session")
        .parent_session_id(Some(SessionId::from("missing-session")))
        .build();
    let members = vec![
        &root_session,
        &parent_session,
        &child_session,
        &orphan_session,
    ];

    // Act
    let child_is_descendant = session_is_descendant_of(&members, &child_session, "root-session");
    let orphan_is_descendant = session_is_descendant_of(&members, &orphan_session, "root-session");

    // Assert
    assert!(child_is_descendant);
    assert!(!orphan_is_descendant);
}

#[test]
fn test_can_mutate_session_branch_in_stack_blocks_parent_with_materialized_child() {
    // Arrange
    let parent_session = SessionFixtureBuilder::new()
        .id("parent-session")
        .draft(false)
        .status(Status::Review)
        .build();
    let child_session = SessionFixtureBuilder::new()
        .id("child-session")
        .draft(true)
        .status(Status::Review)
        .parent_session_id(Some(SessionId::from("parent-session")))
        .build();
    let sessions = vec![parent_session, child_session];

    // Act
    let can_mutate_parent = can_mutate_session_branch_in_stack(&sessions, "parent-session");

    // Assert
    assert!(!can_mutate_parent);
}

#[test]
fn test_can_mutate_session_branch_in_stack_blocks_parent_with_nested_descendant() {
    // Arrange
    let root_session = SessionFixtureBuilder::new()
        .id("root-session")
        .draft(false)
        .status(Status::Review)
        .build();
    let parent_session = SessionFixtureBuilder::new()
        .id("parent-session")
        .draft(true)
        .status(Status::Review)
        .parent_session_id(Some(SessionId::from("root-session")))
        .build();
    let child_session = SessionFixtureBuilder::new()
        .id("child-session")
        .draft(true)
        .status(Status::Review)
        .parent_session_id(Some(SessionId::from("parent-session")))
        .build();
    let sessions = vec![root_session, parent_session, child_session];

    // Act
    let can_mutate_parent = can_mutate_session_branch_in_stack(&sessions, "parent-session");

    // Assert
    assert!(!can_mutate_parent);
}

#[test]
fn test_can_mutate_session_branch_in_stack_blocks_concurrent_stack_member() {
    // Arrange
    let parent_session = SessionFixtureBuilder::new()
        .id("parent-session")
        .draft(false)
        .status(Status::Review)
        .build();
    let running_child_session = SessionFixtureBuilder::new()
        .id("running-child")
        .draft(true)
        .status(Status::InProgress)
        .parent_session_id(Some(SessionId::from("parent-session")))
        .build();
    let review_child_session = SessionFixtureBuilder::new()
        .id("review-child")
        .draft(true)
        .status(Status::Review)
        .parent_session_id(Some(SessionId::from("parent-session")))
        .build();
    let sessions = vec![parent_session, running_child_session, review_child_session];

    // Act
    let can_mutate_review_child = can_mutate_session_branch_in_stack(&sessions, "review-child");

    // Assert
    assert!(!can_mutate_review_child);
}

#[test]
fn test_has_in_progress_timer_returns_true_for_open_interval() {
    // Arrange
    let session = Session {
        base_branch: "main".to_string(),
        created_at: 0,
        draft_attachments: Vec::new(),
        folder: PathBuf::new(),
        follow_up_tasks: Vec::new(),
        id: "session-id".into(),
        in_progress_started_at: Some(120),
        in_progress_total_seconds: 0,
        is_draft: false,
        controller_session_id: None,
        orchestration_progress: None,
        role: SessionRole::default(),
        agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
        parent_session_id: None,
        permission_mode: crate::domain::permission::PermissionMode::AutoEdit,
        personality_id: None,
        project_name: "project".to_string(),
        prompt: String::new(),
        queued_messages: Vec::new(),
        reasoning_level_override: None,
        response_style: crate::domain::agent::ResponseStyle::default(),
        published_upstream_ref: None,
        questions: Vec::new(),
        review_request: None,
        size: SessionSize::Xs,
        speed_mode: SpeedMode::default(),
        stats: SessionStats::default(),
        status: Status::InProgress,
        title: None,
        transcript: None,
        updated_at: 0,
        transient_messages: TransientMessageStore::default(),
    };

    // Act
    let shows_timer = session.has_in_progress_timer();

    // Assert
    assert!(shows_timer);
}

#[test]
fn test_session_id_hash_map_borrowed_lookup() {
    // Arrange
    let session_id = SessionId::from("session-id");
    let sessions = HashMap::from([(session_id, "ready")]);

    // Act
    let status = sessions.get("session-id");

    // Assert
    assert_eq!(status, Some(&"ready"));
}

#[test]
fn test_session_id_serde_serializes_as_plain_string() {
    // Arrange
    let session_id = SessionId::from("session-id");

    // Act
    let serialized_session_id =
        serde_json::to_string(&session_id).expect("session id should serialize");
    let deserialized_session_id: SessionId =
        serde_json::from_str(&serialized_session_id).expect("session id should deserialize");

    // Assert
    assert_eq!(serialized_session_id, "\"session-id\"");
    assert_eq!(deserialized_session_id, session_id);
}

#[test]
fn test_activity_day_key_with_offset_applies_offsets_at_day_boundaries() {
    // Arrange
    let end_of_utc_day = 86_399_i64;
    let start_of_utc_day = 86_400_i64;

    // Act
    let positive_offset_day_key = activity_day_key_with_offset(end_of_utc_day, 3_600);
    let negative_offset_day_key = activity_day_key_with_offset(start_of_utc_day, -3_600);

    // Assert
    assert_eq!(positive_offset_day_key, 1);
    assert_eq!(negative_offset_day_key, 0);
}

#[test]
fn test_can_start_staged_session_in_stack_blocks_active_stack_member() {
    // Arrange
    let parent_session = SessionFixtureBuilder::new()
        .id("parent-session")
        .draft(false)
        .status(Status::Review)
        .build();
    let running_child_session = SessionFixtureBuilder::new()
        .id("running-child")
        .draft(true)
        .status(Status::InProgress)
        .parent_session_id(Some(SessionId::from("parent-session")))
        .build();
    let staged_child_session = SessionFixtureBuilder::new()
        .id("staged-child")
        .draft(true)
        .status(Status::Draft)
        .prompt("Ready child draft")
        .parent_session_id(Some(SessionId::from("parent-session")))
        .build();
    let sessions = vec![parent_session, running_child_session, staged_child_session];

    // Act
    let can_start_child = can_start_staged_session_in_stack(&sessions, "staged-child");

    // Assert
    assert!(!can_start_child);
}

#[test]
fn test_session_stats_line_change_counts_ignore_diff_headers() {
    // Arrange
    let diff = "\
diff --git a/src/lib.rs b/src/lib.rs\nindex 1111111..2222222 100644\n--- a/src/lib.rs\n+++ \
                b/src/lib.rs\n@@ -1,2 +1,3 @@\n-old line\n+new line\n+another line\n";

    // Act
    let (added_lines, deleted_lines) = SessionStats::line_change_counts(diff);

    // Assert
    assert_eq!(added_lines, 2);
    assert_eq!(deleted_lines, 1);
}

#[test]
fn test_session_allows_cancel_action_for_running_session() {
    // Arrange
    let mut session = test_session(None);
    session.status = Status::InProgress;

    // Act
    let allows_cancel_action = session.allows_cancel_action();

    // Assert
    assert!(allows_cancel_action);
}

#[test]
fn test_branch_reservations_follow_stack_membership() {
    // Arrange
    let sessions = vec![
        SessionFixtureBuilder::new().id("root").build(),
        SessionFixtureBuilder::new()
            .id("child")
            .parent_session_id(Some(SessionId::from("root")))
            .build(),
        SessionFixtureBuilder::new()
            .id("grandchild")
            .parent_session_id(Some(SessionId::from("child")))
            .build(),
        SessionFixtureBuilder::new().id("unrelated").build(),
    ];

    // Act
    let root_is_reserved =
        has_reserved_branch_work_in_stack(&sessions, "root", |id| id == "grandchild");
    let child_is_reserved =
        has_reserved_branch_work_in_stack(&sessions, "child", |id| id == "root");
    let unrelated_is_reserved =
        has_reserved_branch_work_in_stack(&sessions, "unrelated", |id| id == "child");
    let missing_is_reserved = has_reserved_branch_work_in_stack(&sessions, "missing", |_| false);

    // Assert
    assert!(root_is_reserved);
    assert!(child_is_reserved);
    assert!(!unrelated_is_reserved);
    assert!(missing_is_reserved);
}

#[test]
fn session_diff_stats_distinguish_empty_and_binary_diffs() {
    // Arrange
    let empty_diff = "";
    let binary_diff = "diff --git a/image.png b/image.png\nBinary files differ\n";

    // Act
    let empty_stats = SessionDiffStats::from_diff(empty_diff);
    let binary_stats = SessionDiffStats::from_diff(binary_diff);

    // Assert
    assert_eq!(empty_stats.diff_state(), SessionDiffState::Empty);
    assert_eq!(binary_stats.diff_state(), SessionDiffState::Present);
}

#[test]
fn test_forge_kind_from_str_github() {
    // Arrange
    let raw_forge_kind = "GitHub";

    // Act
    let forge_kind = raw_forge_kind
        .parse::<ForgeKind>()
        .expect("failed to parse review-request forge");

    // Assert
    assert_eq!(forge_kind, ForgeKind::GitHub);
}

#[test]
fn test_forge_kind_from_str_gitlab() {
    // Arrange
    let raw_forge_kind = "GitLab";

    // Act
    let forge_kind = raw_forge_kind
        .parse::<ForgeKind>()
        .expect("failed to parse review-request forge");

    // Assert
    assert_eq!(forge_kind, ForgeKind::GitLab);
}

#[test]
fn test_can_append_session_to_stack_rejects_invalid_ids_and_source_with_child() {
    // Arrange
    let source_session = SessionFixtureBuilder::new()
        .id("source-session")
        .status(Status::Review)
        .build();
    let child_session = SessionFixtureBuilder::new()
        .id("child-session")
        .parent_session_id(Some(SessionId::from("source-session")))
        .status(Status::Review)
        .build();
    let parent_session = SessionFixtureBuilder::new()
        .id("parent-session")
        .status(Status::Review)
        .build();
    let sessions = vec![source_session, child_session, parent_session];

    // Act
    let same_session = can_append_session_to_stack(&sessions, "source-session", "source-session");
    let missing_source =
        can_append_session_to_stack(&sessions, "missing-session", "parent-session");
    let missing_parent =
        can_append_session_to_stack(&sessions, "source-session", "missing-session");
    let source_with_child =
        can_append_session_to_stack(&sessions, "source-session", "parent-session");

    // Assert
    assert!(!same_session);
    assert!(!missing_source);
    assert!(!missing_parent);
    assert!(!source_with_child);
}

#[test]
fn test_can_append_session_to_stack_rejects_active_parent_stack() {
    // Arrange
    let source_session = SessionFixtureBuilder::new()
        .id("source-session")
        .status(Status::Review)
        .build();
    let parent_session = SessionFixtureBuilder::new()
        .id("parent-session")
        .status(Status::Review)
        .build();
    let running_child = SessionFixtureBuilder::new()
        .id("running-child")
        .parent_session_id(Some(SessionId::from("parent-session")))
        .status(Status::InProgress)
        .build();
    let sessions = vec![source_session, parent_session, running_child];

    // Act
    let can_append = can_append_session_to_stack(&sessions, "source-session", "parent-session");

    // Assert
    assert!(!can_append);
}

#[test]
fn test_can_reply_to_session_in_stack_blocks_active_stack_member() {
    // Arrange
    let parent_session = SessionFixtureBuilder::new()
        .id("parent-session")
        .draft(false)
        .status(Status::Review)
        .build();
    let running_child_session = SessionFixtureBuilder::new()
        .id("running-child")
        .draft(true)
        .status(Status::InProgress)
        .parent_session_id(Some(SessionId::from("parent-session")))
        .build();
    let sessions = vec![parent_session, running_child_session];

    // Act
    let can_reply_to_parent = can_reply_to_session_in_stack(&sessions, "parent-session");

    // Assert
    assert!(!can_reply_to_parent);
}

#[test]
fn test_can_reply_to_session_in_stack_blocks_active_nested_descendant() {
    // Arrange
    let root_session = SessionFixtureBuilder::new()
        .id("root-session")
        .draft(false)
        .status(Status::Review)
        .build();
    let child_session = SessionFixtureBuilder::new()
        .id("child-session")
        .draft(true)
        .status(Status::Review)
        .parent_session_id(Some(SessionId::from("root-session")))
        .build();
    let running_grandchild_session = SessionFixtureBuilder::new()
        .id("grandchild-session")
        .draft(true)
        .status(Status::InProgress)
        .parent_session_id(Some(SessionId::from("child-session")))
        .build();
    let sessions = vec![root_session, child_session, running_grandchild_session];

    // Act
    let can_reply_to_root = can_reply_to_session_in_stack(&sessions, "root-session");

    // Assert
    assert!(!can_reply_to_root);
}
