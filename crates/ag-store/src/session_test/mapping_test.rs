use crate::review::SessionReviewRequestRow;
use crate::session::{SessionJoinRow, SessionListRow};

/// Verifies `SessionJoinRow::into_session_row()` drops partially
/// populated review-request columns instead of surfacing an invalid row
/// model.
#[test]
fn test_session_join_row_ignores_partial_review_request_columns() {
    // Arrange
    let mut session_join_row = SessionJoinRow::fixture_for_test();
    session_join_row.review_request_last_refreshed_at = None;

    // Act
    let session_row = session_join_row.into_session_row();

    // Assert
    assert_eq!(session_row.id, "session-a");
    assert_eq!(session_row.project_id, Some(7));
    assert_eq!(
        session_row.parent_session_id.as_deref(),
        Some("parent-session")
    );
    assert_eq!(session_row.status, "Review");
    assert_eq!(session_row.added_lines, 14);
    assert_eq!(session_row.deleted_lines, 6);
    assert_eq!(session_row.review_request, None);
}

/// Verifies `SessionJoinRow::into_session_row()` maps a fully populated
/// review-request into the public session row model.
#[test]
fn test_session_join_row_maps_review_request_columns() {
    // Arrange
    let session_join_row = SessionJoinRow::fixture_for_test();

    // Act
    let session_row = session_join_row.into_session_row();

    // Assert
    assert_eq!(session_row.id, "session-a");
    assert_eq!(session_row.added_lines, 14);
    assert_eq!(session_row.deleted_lines, 6);
    assert_eq!(session_row.project_id, Some(7));
    assert_eq!(
        session_row.parent_session_id.as_deref(),
        Some("parent-session")
    );
    assert_eq!(
        session_row.published_upstream_ref.as_deref(),
        Some("origin/session-a")
    );
    assert_eq!(session_row.questions.as_deref(), Some("Question text"));
    assert_eq!(session_row.title.as_deref(), Some("Review session"));
    assert_eq!(
        session_row.review_request,
        Some(expected_review_request_row())
    );
}

#[test]
fn list_and_detail_mappings_preserve_all_shared_columns() {
    // Arrange
    let full = SessionJoinRow::fixture_for_test().into_session_row();
    let joined = SessionJoinRow::fixture_for_test();

    // Act
    let from_full = SessionListRow::from(full);
    let from_join = joined.into_session_list_row();

    // Assert
    for row in [from_full, from_join] {
        assert_eq!(row.added_lines, 14);
        assert_eq!(row.agent, "codex");
        assert_eq!(row.base_branch, "main");
        assert_eq!(row.created_at, 100);
        assert_eq!(row.deleted_lines, 6);
        assert_eq!(row.has_diff, Some(true));
        assert_eq!(row.id, "session-a");
        assert_eq!(row.in_progress_started_at, None);
        assert_eq!(row.in_progress_total_seconds, 0);
        assert_eq!(row.input_tokens, 11);
        assert!(!row.is_draft);
        assert_eq!(row.model, "gpt-5.6-sol");
        assert_eq!(row.output_tokens, 29);
        assert_eq!(row.parent_session_id.as_deref(), Some("parent-session"));
        assert_eq!(row.permission_mode, "read_only");

        assert_eq!(row.project_id, Some(7));
        assert_eq!(
            row.published_upstream_ref.as_deref(),
            Some("origin/session-a")
        );
        assert_eq!(row.reasoning_level_override, None);
        assert_eq!(row.response_style, "balanced");
        assert_eq!(row.review_request, Some(expected_review_request_row()));
        assert_eq!(row.size, "M");
        assert_eq!(row.speed_mode, "normal");
        assert_eq!(row.status, "Review");
        assert_eq!(row.title.as_deref(), Some("Review session"));
        assert_eq!(row.updated_at, 200);
    }
}

#[test]
fn full_mapping_keeps_transcript_detail_separate_from_list_metadata() {
    // Arrange
    let joined = SessionJoinRow::fixture_for_test();

    // Act
    let full = joined.into_session_row();

    // Assert
    assert_eq!(full.prompt, "Implement feature");
    assert_eq!(full.questions.as_deref(), Some("Question text"));
}

impl SessionJoinRow {
    /// Builds a deterministic joined-session row fixture for conversion
    /// tests.
    fn fixture_for_test() -> Self {
        Self {
            added_lines: 14,
            agent: "codex".to_string(),
            base_branch: "main".to_string(),
            created_at: 100,
            deleted_lines: 6,
            has_archived_diff: false,
            has_diff: Some(true),
            id: "session-a".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            input_tokens: 11,
            is_draft: false,
            model: "gpt-5.6-sol".to_string(),
            output_tokens: 29,
            parent_session_id: Some("parent-session".to_string()),
            permission_mode: "read_only".to_string(),
            project_id: Some(7),
            prompt: "Implement feature".to_string(),
            published_upstream_ref: Some("origin/session-a".to_string()),
            questions: Some("Question text".to_string()),
            reasoning_level_override: None,
            response_style: "balanced".to_string(),
            review_request_display_id: Some("#42".to_string()),
            review_request_forge_kind: Some("GitHub".to_string()),
            review_request_last_refreshed_at: Some(456),
            review_request_source_branch: Some("feature/forge".to_string()),
            review_request_state: Some("Open".to_string()),
            review_request_status_summary: Some("2 approvals, checks passing".to_string()),
            review_request_target_branch: Some("main".to_string()),
            review_request_title: Some("Add forge review support".to_string()),
            review_request_web_url: Some(
                "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            ),
            size: "M".to_string(),
            speed_mode: "normal".to_string(),
            status: "Review".to_string(),
            title: Some("Review session".to_string()),
            updated_at: 200,
        }
    }
}

/// Builds the fully populated review-request row expected by join-row
/// conversion tests.
fn expected_review_request_row() -> SessionReviewRequestRow {
    SessionReviewRequestRow {
        display_id: "#42".to_string(),
        forge_kind: "GitHub".to_string(),
        last_refreshed_at: 456,
        source_branch: "feature/forge".to_string(),
        state: "Open".to_string(),
        status_summary: Some("2 approvals, checks passing".to_string()),
        target_branch: "main".to_string(),
        title: "Add forge review support".to_string(),
        web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
    }
}
