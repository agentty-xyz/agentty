use std::sync::Arc;

use mockall::Sequence;

use super::support_test::{
    github_empty_pull_request_comments_json, github_oversized_thread_json,
    github_pull_request_comments_json, github_remote, github_review_threads_json,
    github_thread_comment_pages_json, success_output,
};
use crate::client::ReviewRequestAdapter;
use crate::command::MockForgeCommandRunner;
use crate::github::GitHubReviewRequestAdapter;
use crate::model::{ForgeKind, ReviewCommentAnchorSide, ReviewRequestError};

#[tokio::test]
async fn fetch_authenticated_review_comment_snapshot_parses_graphql_response() {
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
                command == &GitHubReviewRequestAdapter::review_threads_command(&remote, "42")
            }
        })
        .returning(|_| Box::pin(async { Ok(success_output(github_review_threads_json())) }));
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| {
                command == &GitHubReviewRequestAdapter::pull_request_comments_command(&remote, "42")
            }
        })
        .returning(|_| Box::pin(async { Ok(success_output(github_pull_request_comments_json())) }));
    let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let snapshot = adapter
        .fetch_authenticated_review_comment_snapshot(remote, "#42".to_string())
        .await
        .expect("GitHub review-comment snapshot fetch should succeed");

    // Assert
    assert_eq!(snapshot.threads.len(), 3);
    let unresolved = &snapshot.threads[0];
    assert_eq!(unresolved.id, "thread-1");
    assert_eq!(unresolved.path, "src/foo.rs");
    assert_eq!(unresolved.line, Some(42));
    assert_eq!(unresolved.anchor_side, ReviewCommentAnchorSide::New);
    assert_eq!(unresolved.is_outdated, Some(false));
    assert!(!unresolved.is_resolved);
    assert_eq!(unresolved.comments.len(), 2);
    assert_eq!(unresolved.comments[0].author, "alice");
    assert!(!unresolved.comments[0].authored_by_current_user);
    assert_eq!(unresolved.comments[0].body, "Why aren't we handling None?");
    assert!(unresolved.comments[1].authored_by_current_user);
    assert!(unresolved.is_addressed_by_agentty());

    let resolved = &snapshot.threads[1];
    assert_eq!(resolved.path, "src/bar.rs");
    assert_eq!(resolved.anchor_side, ReviewCommentAnchorSide::Old);
    assert!(resolved.is_resolved);
    assert_eq!(resolved.comments.len(), 1);
    assert_eq!(resolved.comments[0].author, "ghost");

    let file_level = &snapshot.threads[2];
    assert_eq!(file_level.path, "Cargo.toml");
    assert_eq!(file_level.line, None);
    assert_eq!(file_level.anchor_side, ReviewCommentAnchorSide::File);

    assert_eq!(snapshot.pr_level_comments.len(), 2);
    assert_eq!(snapshot.pr_level_comments[0].author, "carol");
    assert_eq!(snapshot.pr_level_comments[0].body, "Overall looks good.");
    assert_eq!(snapshot.pr_level_comments[1].author, "ghost");
}

#[tokio::test]
async fn fetch_authenticated_review_comment_snapshot_loads_oversized_thread_comments() {
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
                command == &GitHubReviewRequestAdapter::review_threads_command(&remote, "42")
            }
        })
        .returning(|_| Box::pin(async { Ok(success_output(github_oversized_thread_json())) }));
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| {
                command == &GitHubReviewRequestAdapter::pull_request_comments_command(&remote, "42")
            }
        })
        .returning(|_| {
            Box::pin(async { Ok(success_output(github_empty_pull_request_comments_json())) })
        });
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| {
                command
                    == &GitHubReviewRequestAdapter::thread_comments_command(&remote, "thread-large")
            }
        })
        .returning(|_| Box::pin(async { Ok(success_output(github_thread_comment_pages_json())) }));
    let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let snapshot = adapter
        .fetch_authenticated_review_comment_snapshot(remote, "#42".to_string())
        .await
        .expect("oversized review thread should load all comments");

    // Assert
    assert_eq!(snapshot.threads.len(), 1);
    assert_eq!(snapshot.threads[0].comments.len(), 2);
    assert_eq!(snapshot.threads[0].comments[0].body, "First page");
    assert_eq!(snapshot.threads[0].comments[1].body, "Second page");
}

#[tokio::test]
async fn fetch_authenticated_review_comment_snapshot_preserves_thread_parse_failure() {
    // Arrange
    let remote = github_remote();
    let mut command_runner = MockForgeCommandRunner::new();
    command_runner
        .expect_run()
        .once()
        .withf({
            let remote = remote.clone();

            move |command| {
                command == &GitHubReviewRequestAdapter::review_threads_command(&remote, "42")
            }
        })
        .returning(|_| Box::pin(async { Ok(success_output("not-json".to_string())) }));
    let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let error = adapter
        .fetch_authenticated_review_comment_snapshot(remote, "#42".to_string())
        .await
        .expect_err("invalid review threads should fail");

    // Assert
    assert!(matches!(
        error,
        ReviewRequestError::OperationFailed { forge_kind, message }
            if forge_kind == ForgeKind::GitHub
                && message.contains("invalid GitHub review-threads response")
    ));
}

#[tokio::test]
async fn fetch_authenticated_review_comment_snapshot_preserves_pr_comment_parse_failure() {
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
                command == &GitHubReviewRequestAdapter::review_threads_command(&remote, "42")
            }
        })
        .returning(|_| Box::pin(async { Ok(success_output(github_review_threads_json())) }));
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| {
                command == &GitHubReviewRequestAdapter::pull_request_comments_command(&remote, "42")
            }
        })
        .returning(|_| Box::pin(async { Ok(success_output("not-json".to_string())) }));
    let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let error = adapter
        .fetch_authenticated_review_comment_snapshot(remote, "#42".to_string())
        .await
        .expect_err("invalid pull-request comments should fail");

    // Assert
    assert!(matches!(
        error,
        ReviewRequestError::OperationFailed { forge_kind, message }
            if forge_kind == ForgeKind::GitHub
                && message.contains("invalid GitHub pull-request comments response")
    ));
}

#[tokio::test]
async fn fetch_authenticated_review_comment_snapshot_preserves_thread_comment_parse_failure() {
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
                command == &GitHubReviewRequestAdapter::review_threads_command(&remote, "42")
            }
        })
        .returning(|_| Box::pin(async { Ok(success_output(github_oversized_thread_json())) }));
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| {
                command == &GitHubReviewRequestAdapter::pull_request_comments_command(&remote, "42")
            }
        })
        .returning(|_| {
            Box::pin(async { Ok(success_output(github_empty_pull_request_comments_json())) })
        });
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| {
                command
                    == &GitHubReviewRequestAdapter::thread_comments_command(&remote, "thread-large")
            }
        })
        .returning(|_| Box::pin(async { Ok(success_output("not-json".to_string())) }));
    let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let error = adapter
        .fetch_authenticated_review_comment_snapshot(remote, "#42".to_string())
        .await
        .expect_err("invalid review-thread comments should fail");

    // Assert
    assert!(matches!(
        error,
        ReviewRequestError::OperationFailed { forge_kind, message }
            if forge_kind == ForgeKind::GitHub
                && message.contains("invalid GitHub review-thread comments response")
    ));
}

#[tokio::test]
async fn review_thread_reply_and_resolution_run_graphql_mutations() {
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
                command
                    == &GitHubReviewRequestAdapter::reply_to_thread_command(
                        &remote,
                        "thread-1",
                        "Addressed.",
                    )
            }
        })
        .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| {
                command == &GitHubReviewRequestAdapter::resolve_thread_command(&remote, "thread-1")
            }
        })
        .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
    let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let reply_result = adapter
        .reply_to_authenticated_thread(
            remote.clone(),
            "#42".to_string(),
            "thread-1".to_string(),
            "Addressed.".to_string(),
        )
        .await;
    let resolution_result = adapter
        .resolve_authenticated_thread(remote, "#42".to_string(), "thread-1".to_string())
        .await;

    // Assert
    assert_eq!(reply_result, Ok(()));
    assert_eq!(resolution_result, Ok(()));
}

#[test]
fn parse_review_thread_pages_rejects_missing_data() {
    // Arrange
    let stdout = "[{\"data\": null}]";

    // Act
    let error = GitHubReviewRequestAdapter::parse_review_thread_pages(stdout)
        .err()
        .expect("null data payload should be rejected");

    // Assert
    assert!(
        error.contains("missing a data payload"),
        "unexpected error: {error}"
    );
}

#[test]
fn parse_review_thread_pages_rejects_missing_pull_request() {
    // Arrange
    let stdout = "[{\"data\": {\"repository\": {\"pullRequest\": null}}}]";

    // Act
    let error = GitHubReviewRequestAdapter::parse_review_thread_pages(stdout)
        .err()
        .expect("null pull request should be rejected");

    // Assert
    assert!(
        error.contains("missing a pull request"),
        "unexpected error: {error}"
    );
}

#[test]
fn paginated_comment_parsers_reject_missing_graphql_nodes() {
    // Arrange
    let missing_data = "[{\"data\": null}]";
    let missing_pull_request = "[{\"data\": {\"repository\": {\"pullRequest\": null}}}]";
    let missing_thread = "[{\"data\": {\"node\": null}}]";

    // Act
    let pull_request_data_error =
        GitHubReviewRequestAdapter::parse_pull_request_comment_pages(missing_data)
            .expect_err("missing pull-request data should fail");
    let pull_request_error =
        GitHubReviewRequestAdapter::parse_pull_request_comment_pages(missing_pull_request)
            .expect_err("missing pull request should fail");
    let thread_data_error = GitHubReviewRequestAdapter::parse_thread_comment_pages(missing_data)
        .err()
        .expect("missing thread data should fail");
    let thread_error = GitHubReviewRequestAdapter::parse_thread_comment_pages(missing_thread)
        .err()
        .expect("missing thread should fail");

    // Assert
    assert!(pull_request_data_error.contains("missing a data payload"));
    assert!(pull_request_error.contains("missing a pull request"));
    assert!(thread_data_error.contains("missing a data payload"));
    assert!(thread_error.contains("missing a thread"));
}
