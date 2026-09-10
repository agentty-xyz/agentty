use std::path::PathBuf;
use std::sync::Arc;

use mockall::Sequence;

use super::support_test::{
    failure_output, github_empty_pull_request_comments_json, github_remote, github_view_json,
    success_output,
};
use crate::client::ReviewRequestAdapter;
use crate::command::MockForgeCommandRunner;
use crate::github::{GitHubReviewRequestAdapter, GitHubViewResponse};
use crate::model::{
    CreateReviewRequestInput, ForgeKind, ReviewComment, ReviewRequestError, ReviewRequestState,
    ReviewRequestSummary,
};

#[tokio::test]
async fn find_authenticated_by_source_branch_builds_lookup_and_refresh_commands() {
    // Arrange
    let remote = github_remote();
    let mut sequence = Sequence::new();
    let mut command_runner = MockForgeCommandRunner::new();
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| {
                command == &GitHubReviewRequestAdapter::lookup_command(&remote, "feature/forge")
            }
        })
        .returning(|_| Box::pin(async { Ok(success_output(r#"[{"number":42}]"#.to_string())) }));
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| command == &GitHubReviewRequestAdapter::view_command(&remote, "42")
        })
        .returning(|_| Box::pin(async { Ok(success_output(github_view_json())) }));
    let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let review_request = adapter
        .find_authenticated_by_source_branch(remote, "feature/forge".to_string())
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

#[test]
fn lookup_command_limits_lookup_to_open_pull_requests() {
    // Arrange
    let remote = github_remote();

    // Act
    let command = GitHubReviewRequestAdapter::lookup_command(&remote, "feature/forge");

    // Assert
    assert!(command.arguments.contains(&"state=open".to_string()));
    assert!(!command.arguments.contains(&"state=all".to_string()));
}

#[tokio::test]
async fn create_authenticated_review_request_builds_create_command_and_returns_summary() {
    // Arrange
    let remote = github_remote();
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

            move |command| command == &GitHubReviewRequestAdapter::create_command(&remote, &input)
        })
        .returning(|_| {
            Box::pin(async {
                Ok(success_output(
                    "https://github.com/agentty-xyz/agentty/pull/42\n".to_string(),
                ))
            })
        });
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| {
                command == &GitHubReviewRequestAdapter::lookup_command(&remote, "feature/forge")
            }
        })
        .returning(|_| Box::pin(async { Ok(success_output(r#"[{"number":42}]"#.to_string())) }));
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| command == &GitHubReviewRequestAdapter::view_command(&remote, "42")
        })
        .returning(|_| Box::pin(async { Ok(success_output(github_view_json())) }));
    let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let review_request = adapter
        .create_authenticated_review_request(remote, input)
        .await
        .expect("GitHub create should succeed");

    // Assert
    assert_eq!(review_request.display_id, "#42");
    assert_eq!(
        review_request.status_summary.as_deref(),
        Some("Approved, Mergeable")
    );
}

#[test]
fn create_command_marks_pull_requests_as_draft_by_default() {
    // Arrange
    let remote = github_remote();
    let input = CreateReviewRequestInput {
        body: Some("Implements the provider adapters.".to_string()),
        source_branch: "feature/forge".to_string(),
        target_branch: "main".to_string(),
        title: "Add forge review support".to_string(),
    };

    // Act
    let command = GitHubReviewRequestAdapter::create_command(&remote, &input);

    // Assert
    assert_eq!(command.executable, "gh");
    assert!(
        command
            .arguments
            .iter()
            .any(|argument| argument == "--draft")
    );
}

#[test]
fn github_commands_use_remote_working_directory_for_git_context() {
    // Arrange
    let remote =
        github_remote().with_command_working_directory(PathBuf::from("/tmp/session-worktree"));
    let input = CreateReviewRequestInput {
        body: Some("Implements the provider adapters.".to_string()),
        source_branch: "feature/forge".to_string(),
        target_branch: "main".to_string(),
        title: "Add forge review support".to_string(),
    };

    // Act
    let auth_command = GitHubReviewRequestAdapter::auth_status_command(&remote);
    let lookup_command = GitHubReviewRequestAdapter::lookup_command(&remote, "feature/forge");
    let create_command = GitHubReviewRequestAdapter::create_command(&remote, &input);
    let view_command = GitHubReviewRequestAdapter::view_command(&remote, "42");

    // Assert
    assert_eq!(
        auth_command.working_directory,
        Some(PathBuf::from("/tmp/session-worktree"))
    );
    assert_eq!(
        lookup_command.working_directory,
        Some(PathBuf::from("/tmp/session-worktree"))
    );
    assert_eq!(
        create_command.working_directory,
        Some(PathBuf::from("/tmp/session-worktree"))
    );
    assert_eq!(
        view_command.working_directory,
        Some(PathBuf::from("/tmp/session-worktree"))
    );
}

#[test]
fn paginated_snapshot_parsers_return_empty_collections_for_empty_connections() {
    // Arrange
    let thread_stdout = r#"[{"data": {"repository": {"pullRequest": {
        "reviewThreads": {"nodes": [], "pageInfo": {"hasNextPage": false, "endCursor": null}}
    }}}}]"#;
    let comment_stdout = github_empty_pull_request_comments_json();

    // Act
    let threads = GitHubReviewRequestAdapter::parse_review_thread_pages(thread_stdout)
        .expect("empty review thread list should parse");
    let comments = GitHubReviewRequestAdapter::parse_pull_request_comment_pages(&comment_stdout)
        .expect("empty pull-request comments should parse");

    // Assert
    assert!(threads.is_empty());
    assert_eq!(comments, [] as [ReviewComment; 0]);
}

#[test]
fn paginated_snapshot_parsers_preserve_invalid_json_context() {
    // Arrange
    let invalid_json = "not-json";

    // Act
    let thread_error = GitHubReviewRequestAdapter::parse_review_thread_pages(invalid_json)
        .err()
        .expect("invalid review-thread JSON should fail");
    let pull_request_error =
        GitHubReviewRequestAdapter::parse_pull_request_comment_pages(invalid_json)
            .expect_err("invalid pull-request comment JSON should fail");
    let thread_comment_error = GitHubReviewRequestAdapter::parse_thread_comment_pages(invalid_json)
        .err()
        .expect("invalid thread-comment JSON should fail");

    // Assert
    assert!(thread_error.contains("invalid GitHub review-threads response"));
    assert!(pull_request_error.contains("invalid GitHub pull-request comments response"));
    assert!(thread_comment_error.contains("invalid GitHub review-thread comments response"));
}

#[test]
fn snapshot_graphql_commands_request_pagination_and_slurped_pages() {
    // Arrange
    let remote = github_remote();
    let commands = [
        GitHubReviewRequestAdapter::review_threads_command(&remote, "42"),
        GitHubReviewRequestAdapter::pull_request_comments_command(&remote, "42"),
        GitHubReviewRequestAdapter::thread_comments_command(&remote, "thread-1"),
    ];

    // Act
    let all_request_pagination = commands.iter().all(|command| {
        command
            .arguments
            .iter()
            .any(|argument| argument == "--paginate")
            && command
                .arguments
                .iter()
                .any(|argument| argument == "--slurp")
            && command
                .arguments
                .iter()
                .any(|argument| argument.contains("$endCursor"))
    });

    // Assert
    assert!(all_request_pagination);
}

#[tokio::test]
async fn refresh_authenticated_review_request_maps_authentication_error() {
    // Arrange
    let remote = github_remote();
    let mut command_runner = MockForgeCommandRunner::new();
    command_runner
        .expect_run()
        .once()
        .withf({
            let remote = remote.clone();

            move |command| command == &GitHubReviewRequestAdapter::view_command(&remote, "42")
        })
        .returning(|_| {
            Box::pin(async {
                Ok(failure_output(
                    "You are not logged into any GitHub hosts. Run `gh auth login`.".to_string(),
                ))
            })
        });
    let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let error = adapter
        .refresh_authenticated_review_request(remote, "#42".to_string())
        .await
        .expect_err("missing auth should be normalized");

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

#[test]
fn github_status_helpers_map_provider_labels() {
    // Arrange
    let review_decision_cases = [
        (Some("APPROVED"), Some("Approved")),
        (Some("CHANGES_REQUESTED"), Some("Changes requested")),
        (Some("REVIEW_REQUIRED"), Some("Review required")),
        (Some("COMMENTED"), Some("Commented")),
        (None, None),
    ];
    let merge_state_cases = [
        (Some("BLOCKED"), Some("Blocked")),
        (Some("CLEAN"), Some("Mergeable")),
        (Some("DIRTY"), Some("Conflicts")),
        (Some("HAS_HOOKS"), Some("Hooks pending")),
        (Some("UNSTABLE"), Some("Checks pending")),
        (Some("UNKNOWN"), None),
        (Some("BEHIND"), Some("Behind")),
        (None, None),
    ];

    // Act & Assert
    for (status, expected) in review_decision_cases {
        assert_eq!(
            GitHubViewResponse::review_decision_summary(status).as_deref(),
            expected
        );
    }
    for (status, expected) in merge_state_cases {
        assert_eq!(
            GitHubViewResponse::merge_state_summary(status).as_deref(),
            expected
        );
    }
}
