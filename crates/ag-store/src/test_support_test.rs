//! Fixtures shared by persistence unit suites.

use ag_session::{ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary};

/// Builds the review-request domain fixture shared by repository persistence
/// tests.
pub(crate) fn review_request_fixture() -> ReviewRequest {
    ReviewRequest {
        last_refreshed_at: 456,
        summary: ReviewRequestSummary {
            display_id: "#42".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "feature/forge".to_string(),
            state: ReviewRequestState::Open,
            status_summary: Some("2 approvals, checks passing".to_string()),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
        },
    }
}
