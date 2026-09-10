use crate::model::{
    AGENTTY_REVIEW_REPLY_MARKER_PREFIX, ForgeKind, ForgeRemote, ReviewComment,
    ReviewCommentAnchorSide, ReviewCommentThread, ReviewRequestError,
};
use crate::remote::detect_remote;

#[test]
fn review_comment_thread_is_actionable_until_agentty_addresses_latest_feedback() {
    // Arrange
    let actionable = review_comment_thread();
    let mut resolved = review_comment_thread();
    resolved.is_resolved = true;
    let mut outdated = review_comment_thread();
    outdated.is_outdated = Some(true);
    let mut addressed = review_comment_thread();
    addressed.comments.push(ReviewComment {
        author: "agentty".to_string(),
        authored_by_current_user: true,
        body: [
            "No change needed.\n\n",
            AGENTTY_REVIEW_REPLY_MARKER_PREFIX,
            "123e4567-e89b-12d3-a456-426614174000 -->",
        ]
        .concat(),
    });
    let mut followed_up = addressed.clone();
    followed_up.comments.push(ReviewComment {
        author: "reviewer".to_string(),
        authored_by_current_user: false,
        body: "Please reconsider.".to_string(),
    });
    let mut reviewer_marker = review_comment_thread();
    reviewer_marker.comments.push(ReviewComment {
        author: "reviewer".to_string(),
        authored_by_current_user: false,
        body: [
            "Please reconsider.\n\n",
            AGENTTY_REVIEW_REPLY_MARKER_PREFIX,
            "123e4567-e89b-12d3-a456-426614174000 -->",
        ]
        .concat(),
    });

    // Act, Assert
    assert!(actionable.is_actionable());
    assert!(!resolved.is_actionable());
    assert!(outdated.is_actionable());
    assert!(addressed.is_addressed_by_agentty());
    assert!(!addressed.is_actionable());
    assert!(!followed_up.is_addressed_by_agentty());
    assert!(followed_up.is_actionable());
    assert!(!reviewer_marker.is_addressed_by_agentty());
    assert!(reviewer_marker.is_actionable());
}

#[test]
fn review_comment_rejects_malformed_agentty_reply_markers() {
    // Arrange
    let malformed_comments = [
        "Ordinary comment",
        "No separator<!-- agentty review resolution:123e4567-e89b-12d3-a456-426614174000 -->",
        "No terminator\n\n<!-- agentty review resolution:123e4567-e89b-12d3-a456-426614174000",
        "Bad token\n\n<!-- agentty review resolution:not-a-uuid -->",
    ];

    // Act
    let results = malformed_comments.map(|body| ReviewComment {
        author: "agentty".to_string(),
        authored_by_current_user: true,
        body: body.to_string(),
    });

    // Assert
    assert!(results.iter().all(|comment| !comment.is_agentty_reply()));
}

#[test]
fn forge_kind_from_str_gitlab() {
    // Arrange
    let raw_forge_kind = "GitLab";

    // Act
    let forge_kind = raw_forge_kind
        .parse::<ForgeKind>()
        .expect("gitlab forge kind should parse");

    // Assert
    assert_eq!(forge_kind, ForgeKind::GitLab);
    assert_eq!(forge_kind.cli_name(), "glab");
    assert_eq!(forge_kind.review_request_name(), "merge request");
    assert_eq!(forge_kind.review_request_short_name(), "MR");
}

#[test]
fn authentication_required_message_includes_original_cli_error_detail() {
    // Arrange
    let error = ReviewRequestError::AuthenticationRequired {
        detail: Some("HTTP 401 Unauthorized. Run `gh auth login`.".to_string()),
        forge_kind: ForgeKind::GitHub,
        host: "github.com".to_string(),
    };

    // Act
    let message = error.detail_message();

    // Assert
    assert!(message.contains("GitHub review requests require local CLI authentication"));
    assert!(message.contains("Run `gh auth login` and retry."));
    assert!(message.contains("Original `gh` error:"));
    assert!(message.contains("HTTP 401 Unauthorized. Run `gh auth login`."));
    assert!(message.contains("```text"));
}

#[test]
fn authentication_required_message_omits_empty_original_cli_error_detail() {
    // Arrange
    let error = ReviewRequestError::AuthenticationRequired {
        detail: Some("   \n".to_string()),
        forge_kind: ForgeKind::GitHub,
        host: "github.com".to_string(),
    };

    // Act
    let message = error.detail_message();

    // Assert
    assert!(message.contains("Run `gh auth login` and retry."));
    assert!(!message.contains("Original `gh` error:"));
}

#[test]
fn review_request_creation_url_returns_github_compare_link() {
    // Arrange
    let remote = ForgeRemote {
        command_working_directory: None,
        forge_kind: ForgeKind::GitHub,
        host: "github.com".to_string(),
        namespace: "agentty-xyz".to_string(),
        project: "agentty".to_string(),
        repo_url: "git@github.com:agentty-xyz/agentty.git".to_string(),
        web_url: "https://github.com/agentty-xyz/agentty".to_string(),
    };

    // Act
    let url = remote
        .review_request_creation_url("review/custom-branch", "main")
        .expect("github compare URL should be created");

    // Assert
    assert_eq!(
        url,
        "https://github.com/agentty-xyz/agentty/compare/main...review%2Fcustom-branch?expand=1"
    );
}

#[test]
fn review_request_creation_url_rejects_invalid_web_url() {
    // Arrange
    let remote = ForgeRemote {
        command_working_directory: None,
        forge_kind: ForgeKind::GitHub,
        host: "github.com".to_string(),
        namespace: "agentty-xyz".to_string(),
        project: "agentty".to_string(),
        repo_url: "git@github.com:agentty-xyz/agentty.git".to_string(),
        web_url: "not a url".to_string(),
    };

    // Act
    let error = remote
        .review_request_creation_url("review/custom-branch", "main")
        .expect_err("invalid web URL should be rejected");

    // Assert
    assert_eq!(
        error,
        ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitHub,
            message: "repository remote is missing a valid web URL: `not a url`".to_string(),
        }
    );
}

#[test]
fn review_request_creation_url_returns_gitlab_merge_request_link() {
    // Arrange
    let remote = ForgeRemote {
        command_working_directory: None,
        forge_kind: ForgeKind::GitLab,
        host: "gitlab.com".to_string(),
        namespace: "agentty-xyz".to_string(),
        project: "agentty".to_string(),
        repo_url: "git@gitlab.com:agentty-xyz/agentty.git".to_string(),
        web_url: "https://gitlab.com/agentty-xyz/agentty".to_string(),
    };

    // Act
    let url = remote
        .review_request_creation_url("review/custom-branch", "main")
        .expect("gitlab merge-request URL should be created");

    // Assert
    assert_eq!(
        url,
        "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/new?merge_request%5Bsource_branch%5D=review%2Fcustom-branch&merge_request%5Btarget_branch%5D=main"
    );
}

fn review_comment_thread() -> ReviewCommentThread {
    ReviewCommentThread {
        anchor_side: ReviewCommentAnchorSide::New,
        comments: Vec::new(),
        id: "thread-1".to_string(),
        is_outdated: Some(false),
        is_resolved: false,
        line: Some(1),
        path: "src/lib.rs".to_string(),
        start_line: None,
    }
}

#[test]
fn review_request_creation_url_rejects_urls_without_path_segments() {
    // Arrange
    for forge_kind in [ForgeKind::GitHub, ForgeKind::GitLab] {
        let mut remote =
            detect_remote("https://github.com/owner/project").expect("valid remote should parse");
        remote.forge_kind = forge_kind;
        remote.web_url = "mailto:owner@example.com".to_string();

        // Act
        let error = remote
            .review_request_creation_url("feature", "main")
            .expect_err("opaque URL cannot carry review-request path segments");

        // Assert
        assert!(
            matches!(error, ReviewRequestError::OperationFailed { forge_kind: actual, .. }
            if actual == forge_kind)
        );
    }
}

#[test]
fn github_creation_url_without_target_compares_source_branch() {
    // Arrange
    let remote =
        detect_remote("https://github.com/owner/project").expect("valid remote should parse");

    // Act
    let url = remote
        .review_request_creation_url("feature", "  ")
        .expect("empty target should use the source branch");

    // Assert
    assert_eq!(
        url,
        "https://github.com/owner/project/compare/feature?expand=1"
    );
}
