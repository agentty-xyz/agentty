use std::time::Duration;

use crate::adapter_common::{
    ReviewRequestMetadataPlan, ReviewRequestOperations, normalize_provider_label, operation_failed,
    status_summary_parts,
};
use crate::command::ForgeCommandError;
use crate::model::{
    ForgeKind, ForgeRemote, ReviewRequestError, ReviewRequestMetadata,
    ReviewRequestMetadataFieldUpdate, UpdateReviewRequestInput,
};

#[test]
fn metadata_plan_updates_fields_that_still_match_reconciled_values() {
    // Arrange
    let metadata = ReviewRequestMetadata {
        body: "Current body.".to_string(),
        title: "Current title".to_string(),
    };
    let input = UpdateReviewRequestInput {
        body: Some(ReviewRequestMetadataFieldUpdate {
            current: "Current body.".to_string(),
            desired: "New body.".to_string(),
        }),
        title: Some(ReviewRequestMetadataFieldUpdate {
            current: "Current title".to_string(),
            desired: "New title".to_string(),
        }),
    };

    // Act
    let plan = ReviewRequestMetadataPlan::new(&metadata, &input);

    // Assert
    assert_eq!(plan.edit.body.as_deref(), Some("New body."));
    assert_eq!(plan.edit.title.as_deref(), Some("New title"));
    assert!(plan.requires_edit());
}

#[test]
fn metadata_plan_skips_fields_changed_after_reconciliation() {
    // Arrange
    let metadata = ReviewRequestMetadata {
        body: "New remote body.".to_string(),
        title: "New remote title".to_string(),
    };
    let input = UpdateReviewRequestInput {
        body: Some(ReviewRequestMetadataFieldUpdate {
            current: "Earlier body.".to_string(),
            desired: "Desired body.".to_string(),
        }),
        title: Some(ReviewRequestMetadataFieldUpdate {
            current: "Earlier title".to_string(),
            desired: "Desired title".to_string(),
        }),
    };

    // Act
    let plan = ReviewRequestMetadataPlan::new(&metadata, &input);

    // Assert
    assert_eq!(plan.edit.body, None);
    assert_eq!(plan.edit.title, None);
    assert!(!plan.requires_edit());
}

#[test]
fn metadata_plan_omits_unchanged_reconciled_fields() {
    // Arrange
    let metadata = ReviewRequestMetadata {
        body: "Current body.".to_string(),
        title: "Current title".to_string(),
    };
    let input = UpdateReviewRequestInput {
        body: Some(ReviewRequestMetadataFieldUpdate {
            current: "Current body.".to_string(),
            desired: "Current body.".to_string(),
        }),
        title: Some(ReviewRequestMetadataFieldUpdate {
            current: "Current title".to_string(),
            desired: "Current title".to_string(),
        }),
    };

    // Act
    let plan = ReviewRequestMetadataPlan::new(&metadata, &input);

    // Assert
    assert!(!plan.requires_edit());
}

#[test]
fn looks_like_authentication_failure_matches_github_cli_login_prompt() {
    // Arrange
    let detail = "You are not logged into any GitHub hosts. Run `gh auth login`.";

    // Act
    let matched =
        ReviewRequestOperations::looks_like_authentication_failure(detail, ForgeKind::GitHub);

    // Assert
    assert!(matched);
}

#[test]
fn looks_like_authentication_failure_matches_gitlab_cli_login_prompt() {
    // Arrange
    let detail = "You are not logged in. Run `glab auth login`.";

    // Act
    let matched =
        ReviewRequestOperations::looks_like_authentication_failure(detail, ForgeKind::GitLab);

    // Assert
    assert!(matched);
}

#[test]
fn looks_like_authentication_failure_matches_http_401() {
    // Arrange
    let detail = "HTTP 401 Unauthorized";

    // Act
    let matched_github =
        ReviewRequestOperations::looks_like_authentication_failure(detail, ForgeKind::GitHub);
    let matched_gitlab =
        ReviewRequestOperations::looks_like_authentication_failure(detail, ForgeKind::GitLab);

    // Assert
    assert!(matched_github);
    assert!(matched_gitlab);
}

#[test]
fn looks_like_authentication_failure_returns_false_for_unrelated_detail() {
    // Arrange
    let detail = "Request failed: rate limit exceeded";

    // Act
    let matched =
        ReviewRequestOperations::looks_like_authentication_failure(detail, ForgeKind::GitHub);

    // Assert
    assert!(!matched);
}

#[test]
fn looks_like_host_resolution_failure_matches_common_dns_errors() {
    // Arrange
    let details = [
        "dial tcp: lookup github.com: no such host",
        "Name or service not known",
        "Temporary failure in name resolution",
        "Could not resolve host: gitlab.example.internal",
    ];

    // Act & Assert
    for detail in details {
        assert!(
            ReviewRequestOperations::looks_like_host_resolution_failure(detail),
            "expected `{detail}` to match",
        );
    }
}

#[test]
fn looks_like_host_resolution_failure_returns_false_for_unrelated_detail() {
    // Arrange
    let detail = "HTTP 500 Internal Server Error";

    // Act
    let matched = ReviewRequestOperations::looks_like_host_resolution_failure(detail);

    // Assert
    assert!(!matched);
}

#[test]
fn operation_failed_preserves_forge_kind_and_message() {
    // Arrange
    let forge_kind = ForgeKind::GitLab;
    let message = "merge request lookup failed";

    // Act
    let error = operation_failed(forge_kind, message);

    // Assert
    assert_eq!(
        error,
        ReviewRequestError::OperationFailed {
            forge_kind,
            message: message.to_string(),
        }
    );
}

#[test]
fn status_summary_parts_returns_none_for_empty_input() {
    // Arrange
    let parts: Vec<String> = Vec::new();

    // Act
    let summary = status_summary_parts(&parts);

    // Assert
    assert_eq!(summary, None);
}

#[test]
fn status_summary_parts_joins_values_with_commas() {
    // Arrange
    let parts = vec![
        "Draft".to_string(),
        "Approved".to_string(),
        "Mergeable".to_string(),
    ];

    // Act
    let summary = status_summary_parts(&parts);

    // Assert
    assert_eq!(summary.as_deref(), Some("Draft, Approved, Mergeable"));
}

#[test]
fn normalize_provider_label_capitalizes_first_letter_and_replaces_underscores() {
    // Arrange
    let label = "CHANGES_REQUESTED";

    // Act
    let normalized = normalize_provider_label(label);

    // Assert
    assert_eq!(normalized, "Changes requested");
}

#[test]
fn normalize_provider_label_returns_empty_string_for_empty_input() {
    // Arrange
    let label = "";

    // Act
    let normalized = normalize_provider_label(label);

    // Assert
    assert_eq!(normalized, String::new());
}

#[test]
fn map_spawn_error_maps_executable_not_found_to_cli_not_installed() {
    // Arrange
    let remote = sample_remote(ForgeKind::GitHub);
    let error = ForgeCommandError::ExecutableNotFound {
        executable: "gh".to_string(),
    };

    // Act
    let review_request_error = ReviewRequestOperations::map_spawn_error(&remote, error);

    // Assert
    assert_eq!(
        review_request_error,
        ReviewRequestError::CliNotInstalled {
            forge_kind: ForgeKind::GitHub,
        }
    );
}

#[test]
fn map_spawn_error_maps_host_resolution_failure_for_gitlab() {
    // Arrange
    let remote = sample_remote(ForgeKind::GitLab);
    let error = ForgeCommandError::SpawnFailed {
        executable: "glab".to_string(),
        message: "dial tcp: lookup gitlab.example.internal: no such host".to_string(),
    };

    // Act
    let review_request_error = ReviewRequestOperations::map_spawn_error(&remote, error);

    // Assert
    assert_eq!(
        review_request_error,
        ReviewRequestError::HostResolutionFailed {
            forge_kind: ForgeKind::GitLab,
            host: "gitlab.example.internal".to_string(),
        }
    );
}

#[test]
fn map_spawn_error_falls_back_to_operation_failed_with_cli_name() {
    // Arrange
    let remote = sample_remote(ForgeKind::GitHub);
    let error = ForgeCommandError::SpawnFailed {
        executable: "gh".to_string(),
        message: "permission denied".to_string(),
    };

    // Act
    let review_request_error = ReviewRequestOperations::map_spawn_error(&remote, error);

    // Assert
    assert_eq!(
        review_request_error,
        ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitHub,
            message: "failed to execute `gh`: permission denied".to_string(),
        }
    );
}

#[test]
fn map_spawn_error_reports_command_timeout() {
    // Arrange
    let remote = sample_remote(ForgeKind::GitHub);
    let error = ForgeCommandError::TimedOut {
        executable: "gh".to_string(),
        timeout: Duration::from_secs(30),
    };

    // Act
    let review_request_error = ReviewRequestOperations::map_spawn_error(&remote, error);

    // Assert
    assert_eq!(
        review_request_error,
        ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitHub,
            message: "`gh` timed out after 30 seconds while contacting github.com".to_string(),
        }
    );
}

fn sample_remote(forge_kind: ForgeKind) -> ForgeRemote {
    let host = match forge_kind {
        ForgeKind::GitHub => "github.com",
        ForgeKind::GitLab => "gitlab.example.internal",
    };
    ForgeRemote {
        command_working_directory: None,
        forge_kind,
        host: host.to_string(),
        namespace: "agentty-xyz".to_string(),
        project: "agentty".to_string(),
        repo_url: format!("https://{host}/agentty-xyz/agentty.git"),
        web_url: format!("https://{host}/agentty-xyz/agentty"),
    }
}
