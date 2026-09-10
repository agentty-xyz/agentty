use std::path::PathBuf;
use std::sync::Arc;

use mockall::Sequence;

use super::support_test::{gitlab_remote, gitlab_view_json, success_output};
use crate::client::ReviewRequestAdapter;
use crate::command::MockForgeCommandRunner;
use crate::gitlab::{GitLabReviewRequestAdapter, GitLabViewResponse};
use crate::model::{
    CreateReviewRequestInput, ForgeKind, ReviewRequestError, ReviewRequestState,
    ReviewRequestSummary,
};

#[tokio::test]
async fn find_authenticated_by_source_branch_builds_lookup_and_refresh_commands() {
    // Arrange
    let remote = gitlab_remote();
    let mut sequence = Sequence::new();
    let mut command_runner = MockForgeCommandRunner::new();
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| {
                command == &GitLabReviewRequestAdapter::lookup_command(&remote, "feature/forge")
            }
        })
        .returning(|_| Box::pin(async { Ok(success_output(r#"[{"iid":42}]"#.to_string())) }));
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| command == &GitLabReviewRequestAdapter::view_command(&remote, "42")
        })
        .returning(|_| Box::pin(async { Ok(success_output(gitlab_view_json())) }));
    let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let review_request = adapter
        .find_authenticated_by_source_branch(remote, "feature/forge".to_string())
        .await
        .expect("GitLab lookup should succeed");

    // Assert
    assert_eq!(
        review_request,
        Some(ReviewRequestSummary {
            display_id: "!42".to_string(),
            forge_kind: ForgeKind::GitLab,
            source_branch: "feature/forge".to_string(),
            state: ReviewRequestState::Open,
            status_summary: Some("Draft, Mergeable".to_string()),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
            web_url: "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42".to_string(),
        })
    );
}

#[tokio::test]
async fn find_authenticated_by_source_branch_returns_none_for_empty_lookup_response() {
    // Arrange
    let remote = gitlab_remote();
    let mut sequence = Sequence::new();
    let mut command_runner = MockForgeCommandRunner::new();
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| {
                command == &GitLabReviewRequestAdapter::lookup_command(&remote, "feature/forge")
            }
        })
        .returning(|_| Box::pin(async { Ok(success_output("[]".to_string())) }));
    let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let review_request = adapter
        .find_authenticated_by_source_branch(remote, "feature/forge".to_string())
        .await
        .expect("GitLab lookup should succeed");

    // Assert
    assert_eq!(review_request, None);
}

#[test]
fn lookup_command_uses_default_open_merge_request_filter() {
    // Arrange
    let remote = gitlab_remote();

    // Act
    let command = GitLabReviewRequestAdapter::lookup_command(&remote, "feature/forge");

    // Assert
    assert!(!command.arguments.contains(&"--all".to_string()));
    assert!(!command.arguments.contains(&"--closed".to_string()));
    assert!(!command.arguments.contains(&"--merged".to_string()));
    assert!(command.arguments.contains(&"--source-branch".to_string()));
}

#[tokio::test]
async fn create_authenticated_review_request_builds_create_command_and_returns_summary() {
    // Arrange
    let remote = gitlab_remote();
    let input = CreateReviewRequestInput {
        body: Some("Implements the provider adapters.".to_string()),
        source_branch: "feature/forge".to_string(),
        target_branch: "main".to_string(),
        title: "Add forge review support".to_string(),
    };
    let mut sequence = Sequence::new();
    let mut command_runner = MockForgeCommandRunner::new();
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();
            let input = input.clone();

            move |command| command == &GitLabReviewRequestAdapter::create_command(&remote, &input)
        })
        .returning(|_| {
            Box::pin(async {
                Ok(success_output(
                    "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42\n".to_string(),
                ))
            })
        });
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| command == &GitLabReviewRequestAdapter::view_command(&remote, "42")
        })
        .returning(|_| Box::pin(async { Ok(success_output(gitlab_view_json())) }));
    let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let review_request = adapter
        .create_authenticated_review_request(remote, input)
        .await
        .expect("GitLab create should succeed");

    // Assert
    assert_eq!(review_request.display_id, "!42");
    assert_eq!(review_request.forge_kind, ForgeKind::GitLab);
    assert_eq!(
        review_request.web_url,
        "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42"
    );
}

#[test]
fn gitlab_view_response_maps_terminal_states() {
    // Arrange
    let cases = [
        (
            Some("2026-07-16T12:00:00Z"),
            "opened",
            ReviewRequestState::Merged,
        ),
        (None, "merged", ReviewRequestState::Merged),
        (None, "closed", ReviewRequestState::Closed),
        (None, "locked", ReviewRequestState::Closed),
        (None, "opened", ReviewRequestState::Open),
    ];

    // Act & Assert
    for (merged_at, state, expected) in cases {
        let response = GitLabViewResponse {
            draft: false,
            detailed_merge_status: None,
            iid: 42,
            merge_status: None,
            merged_at: merged_at.map(str::to_string),
            source_branch: "feature/forge".to_string(),
            state: state.to_string(),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
            web_url: "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42".to_string(),
        };

        assert_eq!(response.review_request_state(), expected);
    }
}

#[test]
fn gitlab_merge_status_summary_maps_provider_labels() {
    // Arrange
    let cases = [
        (Some("can_be_merged"), None, Some("Mergeable")),
        (Some("mergeable"), None, Some("Mergeable")),
        (Some("cannot_be_merged"), None, Some("Conflicts")),
        (Some("cannot_be_merged_recheck"), None, Some("Checking")),
        (Some("checking"), None, Some("Checking")),
        (Some("unchecked"), None, Some("Checking")),
        (Some("ci_still_running"), None, Some("Checks pending")),
        (Some("commits_status"), None, Some("Checks pending")),
        (Some("ci_must_pass"), None, Some("Checks required")),
        (
            Some("discussions_not_resolved"),
            None,
            Some("Discussions unresolved"),
        ),
        (Some("draft_status"), None, None),
        (Some("not_open"), None, None),
        (Some("needs_rebase"), None, Some("Needs rebase")),
        (
            Some("can_be_merged"),
            Some("cannot_be_merged"),
            Some("Conflicts"),
        ),
        (None, None, None),
    ];

    // Act & Assert
    for (merge_status, detailed_merge_status, expected) in cases {
        assert_eq!(
            GitLabViewResponse::merge_status_summary(merge_status, detailed_merge_status)
                .as_deref(),
            expected
        );
    }
}

#[test]
fn detect_remote_supports_gitlab_hosts() {
    // Arrange
    let repo_url = "https://gitlab.com/agentty-xyz/agentty.git";

    // Act
    let remote =
        GitLabReviewRequestAdapter::detect_remote(repo_url).expect("gitlab remote expected");

    // Assert
    assert_eq!(remote.forge_kind, ForgeKind::GitLab);
    assert_eq!(remote.host, "gitlab.com");
    assert_eq!(remote.project_path(), "agentty-xyz/agentty");
}

#[test]
fn create_command_uses_remote_working_directory_for_glab_git_context() {
    // Arrange
    let remote =
        gitlab_remote().with_command_working_directory(PathBuf::from("/tmp/session-worktree"));
    let input = CreateReviewRequestInput {
        body: Some("Implements the provider adapters.".to_string()),
        source_branch: "feature/forge".to_string(),
        target_branch: "main".to_string(),
        title: "Add forge review support".to_string(),
    };

    // Act
    let command = GitLabReviewRequestAdapter::create_command(&remote, &input);

    // Assert
    assert_eq!(
        command.working_directory,
        Some(PathBuf::from("/tmp/session-worktree"))
    );
    assert!(
        command
            .environment
            .contains(&("GITLAB_HOST".to_string(), "gitlab.com".to_string()))
    );
}

#[tokio::test]
async fn create_authenticated_review_request_stops_when_created_url_is_missing() {
    // Arrange
    let remote = gitlab_remote();
    let input = CreateReviewRequestInput {
        body: None,
        source_branch: "feature/forge".to_string(),
        target_branch: "main".to_string(),
        title: "Add forge review support".to_string(),
    };
    let expected_command = GitLabReviewRequestAdapter::create_command(&remote, &input);
    let mut command_runner = MockForgeCommandRunner::new();
    command_runner
        .expect_run()
        .once()
        .withf(move |command| command == &expected_command)
        .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
    let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let error = adapter
        .create_authenticated_review_request(remote, input)
        .await
        .expect_err("missing creation URL should stop before refreshing");

    // Assert
    assert_eq!(
        error,
        ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitLab,
            message: "missing GitLab merge-request URL in create response".to_string(),
        }
    );
}
