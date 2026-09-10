use crate::github::GitHubReviewRequestAdapter;
use crate::model::{ForgeKind, ReviewRequestError};

#[test]
fn parse_display_id_rejects_invalid_pull_request_reference() {
    // Arrange
    let display_id = "#not-a-number";

    // Act
    let error = GitHubReviewRequestAdapter::parse_display_id(display_id)
        .expect_err("invalid display id should fail");

    // Assert
    assert_eq!(
        error,
        ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitHub,
            message: "invalid GitHub pull-request display id: `#not-a-number`".to_string(),
        }
    );
}

#[test]
fn request_parsers_preserve_invalid_json_context() {
    // Arrange
    let invalid_json = "not JSON";

    // Act
    let errors = [
        GitHubReviewRequestAdapter::parse_lookup_display_id(invalid_json).err(),
        GitHubReviewRequestAdapter::parse_view_response(invalid_json).err(),
        GitHubReviewRequestAdapter::parse_metadata_response(invalid_json).err(),
    ];

    // Assert
    for error in errors {
        assert!(
            error
                .expect("invalid response should fail")
                .starts_with("invalid GitHub")
        );
    }
}
