use std::sync::Arc;

use mockall::Sequence;

use crate::client::{RealReviewRequestClient, ReviewRequestClient};
use crate::command::{ForgeCommand, ForgeCommandOutput, MockForgeCommandRunner};
use crate::model::{
    ForgeKind, ForgeRemote, ReviewRequestError, ReviewRequestMetadata, ReviewRequestState,
    ReviewRequestSummary,
};

#[test]
fn review_request_web_url_returns_error_when_summary_is_missing_url() {
    // Arrange
    let client = RealReviewRequestClient::default();
    let review_request = ReviewRequestSummary {
        display_id: "#42".to_string(),
        forge_kind: ForgeKind::GitHub,
        source_branch: "feature/forge".to_string(),
        state: ReviewRequestState::Open,
        status_summary: Some("Mergeable".to_string()),
        target_branch: "main".to_string(),
        title: "Add forge boundary".to_string(),
        web_url: String::new(),
    };

    // Act
    let error = client
        .review_request_web_url(&review_request)
        .expect_err("missing URL should be rejected");

    // Assert
    assert_eq!(
        error,
        ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitHub,
            message: "review request summary is missing a web URL".to_string(),
        }
    );
}

#[test]
fn review_request_web_url_returns_gitlab_url_without_provider_routing() {
    // Arrange
    let client = RealReviewRequestClient::default();
    let review_request = ReviewRequestSummary {
        display_id: "!42".to_string(),
        forge_kind: ForgeKind::GitLab,
        source_branch: "feature/forge".to_string(),
        state: ReviewRequestState::Open,
        status_summary: Some("Draft".to_string()),
        target_branch: "main".to_string(),
        title: "Add forge boundary".to_string(),
        web_url: "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42".to_string(),
    };

    // Act
    let web_url = client
        .review_request_web_url(&review_request)
        .expect("gitlab review-request URL should be returned directly");

    // Assert
    assert_eq!(
        web_url,
        "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42"
    );
}

#[tokio::test]
async fn find_by_source_branch_authenticates_once_before_github_lookup() {
    // Arrange
    let remote = github_remote();
    let mut sequence = Sequence::new();
    let mut command_runner = MockForgeCommandRunner::new();
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf(|command| {
            command_arguments_are(
                command,
                "gh",
                &["auth", "status", "--hostname", "github.com"],
            )
        })
        .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf(|command| {
            command_arguments_are(
                command,
                "gh",
                &[
                    "api",
                    "--hostname",
                    "github.com",
                    "--method",
                    "GET",
                    "repos/agentty-xyz/agentty/pulls",
                    "-f",
                    "head=agentty-xyz:feature/forge",
                    "-f",
                    "state=open",
                    "-f",
                    "sort=created",
                    "-f",
                    "direction=desc",
                    "-f",
                    "per_page=1",
                ],
            )
        })
        .returning(|_| Box::pin(async { Ok(success_output(r#"[{"number":42}]"#.to_string())) }));
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf(|command| {
            command_arguments_are(
                command,
                "gh",
                &[
                    "pr",
                    "view",
                    "42",
                    "--repo",
                    "agentty-xyz/agentty",
                    "--json",
                    "number,title,state,url,baseRefName,headRefName,isDraft,mergeStateStatus,\
                     reviewDecision,mergedAt",
                ],
            )
        })
        .returning(|_| Box::pin(async { Ok(success_output(github_view_json())) }));
    let client = RealReviewRequestClient::new(Arc::new(command_runner));

    // Act
    let review_request = client
        .find_by_source_branch(remote, "feature/forge".to_string())
        .await
        .expect("GitHub lookup should succeed");

    // Assert
    assert_eq!(
        review_request,
        Some(ReviewRequestSummary {
            display_id: "#42".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "feature/forge".to_string(),
            state: ReviewRequestState::Open,
            status_summary: Some("Approved, Mergeable".to_string()),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
        })
    );
}

#[tokio::test]
async fn review_request_metadata_authenticates_before_github_lookup() {
    // Arrange
    let remote = github_remote();
    let mut sequence = Sequence::new();
    let mut command_runner = MockForgeCommandRunner::new();
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf(|command| {
            command_arguments_are(
                command,
                "gh",
                &["auth", "status", "--hostname", "github.com"],
            )
        })
        .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf(|command| {
            command_arguments_are(
                command,
                "gh",
                &[
                    "pr",
                    "view",
                    "42",
                    "--repo",
                    "agentty-xyz/agentty",
                    "--json",
                    "title,body",
                ],
            )
        })
        .returning(|_| {
            Box::pin(async {
                Ok(success_output(
                    r#"{"title":"Current title","body":"Current body"}"#.to_string(),
                ))
            })
        });
    let client = RealReviewRequestClient::new(Arc::new(command_runner));

    // Act
    let metadata = client
        .review_request_metadata(remote, "#42".to_string())
        .await
        .expect("GitHub metadata lookup should succeed");

    // Assert
    assert_eq!(
        metadata,
        ReviewRequestMetadata {
            body: "Current body".to_string(),
            title: "Current title".to_string(),
        }
    );
}

#[tokio::test]
async fn refresh_review_request_stops_on_github_authentication_error() {
    // Arrange
    let remote = github_remote();
    let mut command_runner = MockForgeCommandRunner::new();
    command_runner
        .expect_run()
        .once()
        .withf(|command| {
            command_arguments_are(
                command,
                "gh",
                &["auth", "status", "--hostname", "github.com"],
            )
        })
        .returning(|_| {
            Box::pin(async {
                Ok(failure_output(
                    "You are not logged into any GitHub hosts. Run `gh auth login`.".to_string(),
                ))
            })
        });
    let client = RealReviewRequestClient::new(Arc::new(command_runner));

    // Act
    let error = client
        .refresh_review_request(remote, "#42".to_string())
        .await
        .expect_err("missing auth should stop before refresh");

    // Assert
    assert_eq!(
        error,
        ReviewRequestError::AuthenticationRequired {
            detail: Some(
                "You are not logged into any GitHub hosts. Run `gh auth login`.".to_string()
            ),
            forge_kind: ForgeKind::GitHub,
            host: "github.com".to_string(),
        }
    );
}

#[tokio::test]
async fn review_thread_mutations_authenticate_and_route_to_github_adapter() {
    // Arrange
    let remote = github_remote();
    let mut sequence = Sequence::new();
    let mut command_runner = MockForgeCommandRunner::new();
    for expected_mutation in ["addPullRequestReviewThreadReply", "resolveReviewThread"] {
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf(|command| {
                command_arguments_are(
                    command,
                    "gh",
                    &["auth", "status", "--hostname", "github.com"],
                )
            })
            .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
        command_runner
            .expect_run()
            .once()
            .in_sequence(&mut sequence)
            .withf(move |command| {
                command.executable == "gh"
                    && command
                        .arguments
                        .iter()
                        .any(|argument| argument.contains(expected_mutation))
            })
            .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
    }
    let client = RealReviewRequestClient::new(Arc::new(command_runner));

    // Act
    let reply_result = client
        .reply_to_thread(
            remote.clone(),
            "#42".to_string(),
            "thread-1".to_string(),
            "Addressed.".to_string(),
        )
        .await;
    let resolution_result = client
        .resolve_thread(remote, "#42".to_string(), "thread-1".to_string())
        .await;

    // Assert
    assert_eq!(reply_result, Ok(()));
    assert_eq!(resolution_result, Ok(()));
}

#[tokio::test]
async fn refresh_review_request_authenticates_before_gitlab_refresh() {
    // Arrange
    let remote = gitlab_remote();
    let mut sequence = Sequence::new();
    let mut command_runner = MockForgeCommandRunner::new();
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf(|command| {
            command_arguments_are(
                command,
                "glab",
                &["auth", "status", "--hostname", "gitlab.com"],
            )
        })
        .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf(|command| {
            command_arguments_are(
                command,
                "glab",
                &[
                    "mr",
                    "view",
                    "42",
                    "--repo",
                    "https://gitlab.com/agentty-xyz/agentty",
                    "--output",
                    "json",
                ],
            )
        })
        .returning(|_| Box::pin(async { Ok(success_output(gitlab_view_json())) }));
    let client = RealReviewRequestClient::new(Arc::new(command_runner));

    // Act
    let review_request = client
        .refresh_review_request(remote, "!42".to_string())
        .await
        .expect("GitLab refresh should succeed");

    // Assert
    assert_eq!(review_request.display_id, "!42");
    assert_eq!(review_request.forge_kind, ForgeKind::GitLab);
}

/// Returns whether `command` exactly matches one expected CLI invocation.
fn command_arguments_are(
    command: &ForgeCommand,
    executable: &'static str,
    arguments: &[&str],
) -> bool {
    let expected_arguments = arguments
        .iter()
        .map(|argument| (*argument).to_string())
        .collect::<Vec<_>>();

    command.executable == executable && command.arguments == expected_arguments
}

/// Builds one normalized GitHub remote for client routing tests.
fn github_remote() -> ForgeRemote {
    ForgeRemote {
        command_working_directory: None,
        forge_kind: ForgeKind::GitHub,
        host: "github.com".to_string(),
        namespace: "agentty-xyz".to_string(),
        project: "agentty".to_string(),
        repo_url: "https://github.com/agentty-xyz/agentty.git".to_string(),
        web_url: "https://github.com/agentty-xyz/agentty".to_string(),
    }
}

/// Builds one normalized GitLab remote for client routing tests.
fn gitlab_remote() -> ForgeRemote {
    ForgeRemote {
        command_working_directory: None,
        forge_kind: ForgeKind::GitLab,
        host: "gitlab.com".to_string(),
        namespace: "agentty-xyz".to_string(),
        project: "agentty".to_string(),
        repo_url: "https://gitlab.com/agentty-xyz/agentty.git".to_string(),
        web_url: "https://gitlab.com/agentty-xyz/agentty".to_string(),
    }
}

/// Builds one successful command output with `stdout`.
fn success_output(stdout: String) -> ForgeCommandOutput {
    ForgeCommandOutput {
        exit_code: Some(0),
        stderr: String::new(),
        stdout,
    }
}

/// Builds one failed command output with `stderr`.
fn failure_output(stderr: String) -> ForgeCommandOutput {
    ForgeCommandOutput {
        exit_code: Some(1),
        stderr,
        stdout: String::new(),
    }
}

/// Returns one representative GitHub pull-request JSON response.
fn github_view_json() -> String {
    r#"{
        "number": 42,
        "title": "Add forge review support",
        "state": "OPEN",
        "url": "https://github.com/agentty-xyz/agentty/pull/42",
        "baseRefName": "main",
        "headRefName": "feature/forge",
        "isDraft": false,
        "mergeStateStatus": "CLEAN",
        "reviewDecision": "APPROVED",
        "mergedAt": null
    }"#
    .to_string()
}

/// Returns one representative GitLab merge-request JSON response.
fn gitlab_view_json() -> String {
    r#"{
        "draft": true,
        "detailed_merge_status": "can_be_merged",
        "iid": 42,
        "merge_status": "can_be_merged",
        "merged_at": null,
        "source_branch": "feature/forge",
        "state": "opened",
        "target_branch": "main",
        "title": "Add forge review support",
        "description": "Current description.",
        "web_url": "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42"
    }"#
    .to_string()
}
