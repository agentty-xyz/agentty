//! Session timer, forge indicator, identifier, and diff statistics tests.

use std::collections::HashMap;
use std::path::PathBuf;

use ag_contracts::{SessionDiffState, SessionStats, SpeedMode};
use ag_session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary, SessionId,
    SessionStatus as Status, activity_day_key_with_offset,
};

use super::super::super::agent::AgentSelection;
use super::super::{Session, SessionDiffStats, SessionSize};
use super::support::test_session;
use crate::domain::agent::AgentModel;
use crate::domain::transient_message::TransientMessageStore;

#[test]
fn test_in_progress_duration_seconds_accumulates_closed_and_open_intervals() {
    // Arrange
    let session = Session {
        base_branch: "main".to_string(),
        created_at: 0,
        draft_attachments: Vec::new(),
        folder: PathBuf::new(),
        follow_up_tasks: Vec::new(),
        has_archived_diff: false,
        id: "session-id".into(),
        in_progress_started_at: Some(200),
        in_progress_total_seconds: 90,
        is_draft: false,
        agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
        permission_mode: crate::domain::permission::PermissionMode::AutoEdit,
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
fn test_has_in_progress_timer_returns_true_for_open_interval() {
    // Arrange
    let session = Session {
        base_branch: "main".to_string(),
        created_at: 0,
        draft_attachments: Vec::new(),
        folder: PathBuf::new(),
        follow_up_tasks: Vec::new(),
        has_archived_diff: false,
        id: "session-id".into(),
        in_progress_started_at: Some(120),
        in_progress_total_seconds: 0,
        is_draft: false,
        agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
        permission_mode: crate::domain::permission::PermissionMode::AutoEdit,
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
fn session_diff_stats_distinguish_empty_and_binary_diffs() {
    // Arrange
    let empty_diff = SessionDiffStats::from_line_counts(0, 0, false);
    let binary_only_diff = SessionDiffStats::from_line_counts(0, 0, true);

    // Act
    let empty_availability = empty_diff.diff_state();
    let binary_availability = binary_only_diff.diff_state();

    // Assert
    assert_eq!(empty_availability, SessionDiffState::Empty);
    assert_eq!(binary_availability, SessionDiffState::Present);
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
