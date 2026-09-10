use crate::SessionRow;
use crate::connection::Database;

/// Asserts that one loaded session row carries the expected review-request
/// linkage.
pub(super) fn assert_review_request_row(row: &SessionRow) {
    assert_eq!(
        row.review_request
            .as_ref()
            .map(|review_request| review_request.display_id.as_str()),
        Some("#42")
    );
    assert_eq!(
        row.review_request
            .as_ref()
            .map(|review_request| review_request.forge_kind.as_str()),
        Some("GitHub")
    );
    assert_eq!(
        row.review_request
            .as_ref()
            .map(|review_request| review_request.last_refreshed_at),
        Some(456)
    );
    assert_eq!(
        row.review_request
            .as_ref()
            .map(|review_request| review_request.source_branch.as_str()),
        Some("feature/forge")
    );
    assert_eq!(
        row.review_request
            .as_ref()
            .map(|review_request| review_request.state.as_str()),
        Some("Open")
    );
    assert_eq!(
        row.review_request
            .as_ref()
            .and_then(|review_request| review_request.status_summary.as_deref()),
        Some("2 approvals, checks passing")
    );
    assert_eq!(
        row.review_request
            .as_ref()
            .map(|review_request| review_request.target_branch.as_str()),
        Some("main")
    );
    assert_eq!(
        row.review_request
            .as_ref()
            .map(|review_request| review_request.title.as_str()),
        Some("Add forge review support")
    );
    assert_eq!(
        row.review_request
            .as_ref()
            .map(|review_request| review_request.web_url.as_str()),
        Some("https://github.com/agentty-xyz/agentty/pull/42")
    );
}

/// Inserts one session row with deterministic defaults for tests.
pub(super) async fn insert_session_fixture(
    database: &Database,
    session_id: &str,
    base_branch: &str,
    status: &str,
    project_id: i64,
) {
    database
        .sessions()
        .insert_session(session_id, "gpt-5.6-sol", base_branch, status, project_id)
        .await
        .expect("failed to insert session fixture");
}

/// Loads one session row by identifier through `load_sessions()`.
pub(super) async fn load_session_row(database: &Database, session_id: &str) -> SessionRow {
    database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load all sessions")
        .into_iter()
        .find(|row| row.id == session_id)
        .expect("missing session row")
}
