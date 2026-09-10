use crate::gitlab::GitLabReviewRequestAdapter;
use crate::model::{ForgeKind, ReviewRequestError};

#[test]
fn parse_display_id_rejects_invalid_merge_request_reference() {
    // Arrange
    let display_id = "!not-a-number";

    // Act
    let error = GitLabReviewRequestAdapter::parse_display_id(display_id)
        .expect_err("invalid display id should fail");

    // Assert
    assert_eq!(
        error,
        ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitLab,
            message: "invalid GitLab merge-request display id: `!not-a-number`".to_string(),
        }
    );
}

#[test]
fn parse_create_display_id_reads_merge_request_iid_from_created_url() {
    // Arrange
    let stdout = "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42\n";

    // Act
    let display_id = GitLabReviewRequestAdapter::parse_create_display_id(stdout)
        .expect("create output should parse");

    // Assert
    assert_eq!(display_id, "!42");
}

#[test]
fn parse_create_display_id_rejects_non_numeric_iid() {
    // Arrange
    let stdout = "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/not-a-number\n";

    // Act
    let error = GitLabReviewRequestAdapter::parse_create_display_id(stdout)
        .expect_err("non-numeric iid should fail");

    // Assert
    assert_eq!(
        error,
        "invalid GitLab merge-request display id: `!not-a-number`"
    );
}

#[test]
fn parse_create_display_id_reports_incomplete_or_malformed_urls() {
    // Arrange
    let invalid_responses = [
        ("", "missing GitLab merge-request URL"),
        (
            "not a URL",
            "invalid GitLab merge-request create response URL",
        ),
        (
            "mailto:review@example.com",
            "invalid GitLab merge-request create response URL path",
        ),
        (
            "https://gitlab.com/owner/project",
            "missing merge request path segment",
        ),
        (
            "https://gitlab.com/owner/project/-/merge_requests",
            "missing merge request iid",
        ),
    ];

    for (response, expected) in invalid_responses {
        // Act
        let error = GitLabReviewRequestAdapter::parse_create_display_id(response)
            .expect_err("invalid creation response should be rejected");

        // Assert
        assert!(error.contains(expected), "unexpected parser error: {error}");
    }
}

#[test]
fn request_parsers_preserve_invalid_json_context() {
    // Arrange
    let invalid_json = "not JSON";

    // Act
    let errors = [
        GitLabReviewRequestAdapter::parse_lookup_display_id(invalid_json).err(),
        GitLabReviewRequestAdapter::parse_view_response(invalid_json).err(),
        GitLabReviewRequestAdapter::parse_metadata_response(invalid_json).err(),
        GitLabReviewRequestAdapter::parse_review_comment_snapshot_response(invalid_json, 1).err(),
    ];

    // Assert
    for error in errors {
        assert!(
            error
                .expect("invalid response should fail")
                .starts_with("invalid GitLab")
        );
    }
}
