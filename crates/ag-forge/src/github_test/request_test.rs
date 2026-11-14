use std::path::PathBuf;
use std::sync::Arc;

use mockall::Sequence;

use super::support_test::{
    failure_output, github_empty_pull_request_comments_json, github_remote, github_view_json,
    parse_view_summary, success_output,
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
            status_summary: Some("Approved, Mergeable, PR ready".to_string()),
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

#[test]
fn status_command_requests_compact_readiness_fields() {
    // Arrange
    let remote = github_remote();

    // Act
    let command = GitHubReviewRequestAdapter::view_command(&remote, "42");
    let query = command
        .arguments
        .iter()
        .find_map(|argument| argument.strip_prefix("query="))
        .expect("status command should include a GraphQL query");

    // Assert
    assert_eq!(
        &command.arguments[..4],
        ["api", "--hostname", "github.com", "graphql"]
    );
    assert!(query.contains("mergeable mergeStateStatus"));
    assert!(!query.contains("comments"));
    assert!(query.contains("reviewThreads(first: 100) { totalCount nodes { isResolved } }"));
    assert!(!query.contains("reviews("));
    assert!(query.contains(
        "statusCheckRollup { state contexts(first: 100) { nodes { ... on CheckRun { name } ... on \
         StatusContext { context } } pageInfo { hasNextPage } } }"
    ));
    assert!(
        query.contains("baseRef { refUpdateRule { requiredStatusCheckContexts } rules(first: 100)")
    );
    assert!(query.contains("RequiredStatusChecksParameters { requiredStatusChecks { context } }"));
    assert!(query.contains("viewerCanMergeAsAdmin"));
    assert!(command.arguments.contains(&"number=42".to_string()));
    assert!(!command.arguments.contains(&"--paginate".to_string()));
}

#[test]
fn github_status_marks_only_open_clean_pr_ready() {
    // Arrange
    let ready: serde_json::Value =
        serde_json::from_str(&github_view_json()).expect("fixture should be valid JSON");
    let unready_cases = [
        (
            "/data/repository/pullRequest/isDraft",
            serde_json::json!(true),
        ),
        (
            "/data/repository/pullRequest/state",
            serde_json::json!("MERGED"),
        ),
        (
            "/data/repository/pullRequest/state",
            serde_json::json!("CLOSED"),
        ),
        (
            "/data/repository/pullRequest/mergeStateStatus",
            serde_json::json!("BLOCKED"),
        ),
        (
            "/data/repository/pullRequest/mergeStateStatus",
            serde_json::json!("UNSTABLE"),
        ),
        (
            "/data/repository/pullRequest/mergeStateStatus",
            serde_json::Value::Null,
        ),
    ];

    // Act
    let ready_summary =
        parse_view_summary(&ready.to_string()).expect("ready response should parse");
    let unready_summaries = unready_cases.map(|(path, value)| {
        let mut response = ready.clone();
        *response
            .pointer_mut(path)
            .expect("fixture path should exist") = value;

        parse_view_summary(&response.to_string()).expect("unready response should parse")
    });

    // Assert
    assert!(ready_summary.is_github_pr_ready());
    assert_eq!(
        [unready_summaries[1].state, unready_summaries[2].state],
        [ReviewRequestState::Merged, ReviewRequestState::Closed]
    );
    assert!(
        unready_summaries
            .iter()
            .all(|summary| !summary.is_github_pr_ready())
    );
}

#[test]
fn github_clean_status_without_required_review_is_ready() {
    // Arrange
    let mut response: serde_json::Value =
        serde_json::from_str(&github_view_json()).expect("fixture should be valid JSON");
    response["data"]["repository"]["pullRequest"]["reviewDecision"] = serde_json::Value::Null;

    // Act
    let summary = parse_view_summary(&response.to_string()).expect("clean response should parse");

    // Assert
    assert!(summary.is_github_pr_ready());
    assert_eq!(
        summary.status_summary.as_deref(),
        Some("Mergeable, PR ready")
    );
}

#[test]
fn github_status_marks_bypass_ready_only_when_admin_approval_is_missing() {
    // Arrange
    let mut bypass_ready: serde_json::Value =
        serde_json::from_str(&github_view_json()).expect("fixture should be valid JSON");
    let pull_request = &mut bypass_ready["data"]["repository"]["pullRequest"];
    pull_request["mergeStateStatus"] = serde_json::json!("BLOCKED");
    pull_request["reviewDecision"] = serde_json::json!("REVIEW_REQUIRED");
    set_passing_required_checks(pull_request);
    pull_request["viewerCanMergeAsAdmin"] = serde_json::json!(true);
    let unready_cases = [
        (
            "/data/repository/pullRequest/viewerCanMergeAsAdmin",
            serde_json::json!(false),
        ),
        (
            "/data/repository/pullRequest/reviewDecision",
            serde_json::json!("CHANGES_REQUESTED"),
        ),
        (
            "/data/repository/pullRequest/statusCheckRollup",
            serde_json::json!({"state": "PENDING"}),
        ),
        (
            "/data/repository/pullRequest/statusCheckRollup",
            serde_json::Value::Null,
        ),
        (
            "/data/repository/pullRequest/isDraft",
            serde_json::json!(true),
        ),
        (
            "/data/repository/pullRequest/mergeStateStatus",
            serde_json::json!("DIRTY"),
        ),
    ];

    // Act
    let bypass_summary =
        parse_view_summary(&bypass_ready.to_string()).expect("bypass-ready response should parse");
    let unready_summaries = unready_cases.map(|(path, value)| {
        let mut response = bypass_ready.clone();
        *response
            .pointer_mut(path)
            .expect("fixture path should exist") = value;

        parse_view_summary(&response.to_string()).expect("unready response should parse")
    });

    // Assert
    assert!(bypass_summary.is_github_pr_bypass_ready());
    assert!(!bypass_summary.is_github_pr_ready());
    assert_eq!(
        bypass_summary.status_summary.as_deref(),
        Some("Review required, Blocked, PR ready with bypass")
    );
    assert!(
        unready_summaries
            .iter()
            .all(|summary| !summary.is_github_pr_bypass_ready() && !summary.is_github_pr_ready())
    );
}

#[test]
fn github_status_marks_conflict_free_draft_without_open_threads_bypass_ready() {
    // Arrange
    let mut bypass_ready: serde_json::Value =
        serde_json::from_str(&github_view_json()).expect("fixture should be valid JSON");
    let pull_request = &mut bypass_ready["data"]["repository"]["pullRequest"];
    pull_request["isDraft"] = serde_json::json!(true);
    pull_request["mergeable"] = serde_json::json!("MERGEABLE");
    pull_request["mergeStateStatus"] = serde_json::json!("BEHIND");
    pull_request["reviewDecision"] = serde_json::json!("REVIEW_REQUIRED");
    pull_request["reviewThreads"] = serde_json::json!({
        "totalCount": 1,
        "nodes": [{"isResolved": true}]
    });
    set_passing_required_checks(pull_request);
    pull_request["viewerCanMergeAsAdmin"] = serde_json::json!(true);
    let ready_cases = [
        (
            "/data/repository/pullRequest/reviewThreads",
            serde_json::json!({"totalCount": 0, "nodes": []}),
        ),
        (
            "/data/repository/pullRequest/reviewDecision",
            serde_json::json!("CHANGES_REQUESTED"),
        ),
        (
            "/data/repository/pullRequest/reviewDecision",
            serde_json::Value::Null,
        ),
    ];
    let unready_cases = [
        (
            "/data/repository/pullRequest/mergeable",
            serde_json::json!("CONFLICTING"),
        ),
        (
            "/data/repository/pullRequest/mergeable",
            serde_json::json!("UNKNOWN"),
        ),
        (
            "/data/repository/pullRequest/reviewThreads/nodes/0/isResolved",
            serde_json::json!(false),
        ),
        (
            "/data/repository/pullRequest/reviewThreads/totalCount",
            serde_json::json!(101),
        ),
        (
            "/data/repository/pullRequest/reviewThreads",
            serde_json::Value::Null,
        ),
        (
            "/data/repository/pullRequest/statusCheckRollup",
            serde_json::json!({"state": "FAILURE"}),
        ),
        (
            "/data/repository/pullRequest/viewerCanMergeAsAdmin",
            serde_json::json!(false),
        ),
        (
            "/data/repository/pullRequest/state",
            serde_json::json!("CLOSED"),
        ),
    ];

    // Act
    let bypass_summary = parse_view_summary(&bypass_ready.to_string())
        .expect("draft bypass-ready response should parse");
    let parse_variant = |(path, value): (&str, serde_json::Value)| {
        let mut response = bypass_ready.clone();
        *response
            .pointer_mut(path)
            .expect("fixture path should exist") = value;

        parse_view_summary(&response.to_string()).expect("draft response variant should parse")
    };
    let ready_summaries = ready_cases.map(parse_variant);
    let unready_summaries = unready_cases.map(parse_variant);

    // Assert
    assert!(bypass_summary.is_github_pr_bypass_ready());
    assert!(!bypass_summary.is_github_pr_ready());
    assert_eq!(
        bypass_summary.status_summary.as_deref(),
        Some("Draft, Review required, Behind, PR ready with bypass")
    );
    assert!(
        ready_summaries
            .iter()
            .all(|summary| summary.is_github_pr_bypass_ready() && !summary.is_github_pr_ready())
    );
    assert!(
        unready_summaries
            .iter()
            .all(|summary| !summary.is_github_pr_bypass_ready() && !summary.is_github_pr_ready())
    );
}

#[test]
fn github_status_withholds_bypass_ready_until_required_checks_report() {
    // Arrange
    let bypass_ready = bypass_ready_view();
    let ready_cases = [(
        "/data/repository/pullRequest/baseRef",
        serde_json::json!({"refUpdateRule": null, "rules": {"totalCount": 0, "nodes": []}}),
    )];
    let unready_cases = [
        (
            "/data/repository/pullRequest/statusCheckRollup/contexts/nodes",
            serde_json::json!([{"name": "Workspace validation"}]),
        ),
        (
            "/data/repository/pullRequest/statusCheckRollup/contexts",
            serde_json::Value::Null,
        ),
        (
            "/data/repository/pullRequest/baseRef/refUpdateRule/requiredStatusCheckContexts",
            serde_json::json!(["codecov/patch", "SonarCloud Code Analysis"]),
        ),
        (
            "/data/repository/pullRequest/baseRef/rules/totalCount",
            serde_json::json!(101),
        ),
        (
            "/data/repository/pullRequest/baseRef/rules",
            serde_json::Value::Null,
        ),
        (
            "/data/repository/pullRequest/baseRef",
            serde_json::Value::Null,
        ),
    ];

    // Act
    let bypass_summary =
        parse_view_summary(&bypass_ready.to_string()).expect("bypass-ready response should parse");
    let parse_variant = |(path, value): (&str, serde_json::Value)| {
        let mut response = bypass_ready.clone();
        *response
            .pointer_mut(path)
            .expect("fixture path should exist") = value;

        parse_view_summary(&response.to_string()).expect("required-check variant should parse")
    };
    let ready_summaries = ready_cases.map(parse_variant);
    let unready_summaries = unready_cases.map(parse_variant);

    // Assert
    assert!(bypass_summary.is_github_pr_bypass_ready());
    assert!(
        ready_summaries
            .iter()
            .all(ReviewRequestSummary::is_github_pr_bypass_ready)
    );
    assert!(
        unready_summaries
            .iter()
            .all(|summary| !summary.is_github_pr_bypass_ready())
    );
}

#[tokio::test]
async fn refresh_fetches_remaining_status_contexts_for_unseen_required_checks() {
    // Arrange
    let context_pages = serde_json::json!([
        status_contexts_page(
            "SUCCESS",
            &serde_json::json!([{"name": "Workspace validation"}])
        ),
        status_contexts_page(
            "SUCCESS",
            &serde_json::json!([{"context": "codecov/patch"}])
        ),
    ]);

    // Act
    let summary = refresh_with_status_context_pages(context_pages).await;

    // Assert
    assert!(summary.is_github_pr_bypass_ready());
}

#[tokio::test]
async fn refresh_withholds_bypass_when_rollup_stops_passing_during_pagination() {
    // Arrange
    let context_pages = serde_json::json!([
        status_contexts_page(
            "SUCCESS",
            &serde_json::json!([{"name": "Workspace validation"}])
        ),
        status_contexts_page(
            "PENDING",
            &serde_json::json!([{"context": "codecov/patch"}])
        ),
        status_contexts_page("SUCCESS", &serde_json::json!([])),
    ]);

    // Act
    let summary = refresh_with_status_context_pages(context_pages).await;

    // Assert
    assert!(!summary.is_github_pr_bypass_ready());
}

#[test]
fn status_contexts_command_requests_rollup_state_with_each_page() {
    // Arrange
    let remote = github_remote();

    // Act
    let command = GitHubReviewRequestAdapter::status_contexts_command(&remote, "42");

    // Assert
    assert!(command.arguments.iter().any(|argument| {
        argument.contains("statusCheckRollup { state contexts(first: 100, after: $endCursor)")
    }));
}

#[test]
fn status_context_pages_reject_missing_rollup_or_pages() {
    // Arrange
    let missing_rollup = r#"[{"data":{"repository":{"pullRequest":{"statusCheckRollup":null}}}}]"#;

    // Act
    let missing_rollup_error =
        GitHubReviewRequestAdapter::parse_status_check_rollup_pages(missing_rollup)
            .err()
            .expect("missing rollup should fail");
    let empty_error = GitHubReviewRequestAdapter::parse_status_check_rollup_pages("[]")
        .err()
        .expect("empty pages should fail");

    // Assert
    assert!(missing_rollup_error.contains("missing a status check rollup"));
    assert!(empty_error.contains("contains no pages"));
}

/// Refreshes a bypass-ready pull request whose first contexts page lacks a
/// required check, returning `context_pages` from the paginated follow-up.
async fn refresh_with_status_context_pages(
    context_pages: serde_json::Value,
) -> ReviewRequestSummary {
    let remote = github_remote();
    let mut first_page = bypass_ready_view();
    first_page["data"]["repository"]["pullRequest"]["statusCheckRollup"]["contexts"] = serde_json::json!({
        "nodes": [{"name": "Workspace validation"}],
        "pageInfo": {"hasNextPage": true}
    });
    let mut sequence = Sequence::new();
    let mut command_runner = MockForgeCommandRunner::new();
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| command == &GitHubReviewRequestAdapter::view_command(&remote, "42")
        })
        .returning(move |_| {
            let stdout = first_page.to_string();

            Box::pin(async move { Ok(success_output(stdout)) })
        });
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| {
                command == &GitHubReviewRequestAdapter::status_contexts_command(&remote, "42")
            }
        })
        .returning(move |_| {
            let stdout = context_pages.to_string();

            Box::pin(async move { Ok(success_output(stdout)) })
        });
    let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

    adapter
        .refresh_authenticated_review_request(remote, "#42".to_string())
        .await
        .expect("refresh should fetch the remaining status contexts")
}

/// Builds a status response that is bypass-ready because only a required
/// approval blocks it and every required check passed.
fn bypass_ready_view() -> serde_json::Value {
    let mut bypass_ready: serde_json::Value =
        serde_json::from_str(&github_view_json()).expect("fixture should be valid JSON");
    let pull_request = &mut bypass_ready["data"]["repository"]["pullRequest"];
    pull_request["mergeStateStatus"] = serde_json::json!("BLOCKED");
    pull_request["reviewDecision"] = serde_json::json!("REVIEW_REQUIRED");
    pull_request["viewerCanMergeAsAdmin"] = serde_json::json!(true);
    set_passing_required_checks(pull_request);

    bypass_ready
}

/// Builds one slurped page of the paginated rollup-contexts query with the
/// rollup `state` observed when that page was fetched.
fn status_contexts_page(state: &str, nodes: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({"data": {"repository": {"pullRequest": {"statusCheckRollup": {
        "state": state,
        "contexts": {"nodes": nodes, "pageInfo": {"hasNextPage": false, "endCursor": null}}
    }}}}})
}

/// Marks every reported check as passing and every base-branch required
/// check, from both a ruleset and classic branch protection, as reported.
fn set_passing_required_checks(pull_request: &mut serde_json::Value) {
    pull_request["statusCheckRollup"] = serde_json::json!({
        "state": "SUCCESS",
        "contexts": {"nodes": [
            {"name": "Workspace validation"},
            {"context": "codecov/patch"},
            {}
        ]}
    });
    pull_request["baseRef"] = serde_json::json!({
        "refUpdateRule": {"requiredStatusCheckContexts": ["codecov/patch"]},
        "rules": {"totalCount": 3, "nodes": [
            {"parameters": {"requiredStatusChecks": [{"context": "Workspace validation"}]}},
            {"parameters": {}},
            {"parameters": null}
        ]}
    });
}

#[test]
fn github_status_rejects_missing_pull_request() {
    // Arrange
    let response = r#"{"data":{"repository":{"pullRequest":null}}}"#;

    // Act
    let error = parse_view_summary(response).expect_err("missing pull request should fail");

    // Assert
    assert!(error.contains("missing a pull request"));
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
        Some("Approved, Mergeable, PR ready")
    );
}

#[tokio::test]
async fn create_authenticated_review_request_fails_when_created_pull_request_is_not_found() {
    // Arrange
    let remote = github_remote();
    let input = CreateReviewRequestInput {
        body: None,
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
        .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
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
        .returning(|_| Box::pin(async { Ok(success_output("[]".to_string())) }));
    let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let error = adapter
        .create_authenticated_review_request(remote, input)
        .await
        .expect_err("missing reloaded pull request should fail");

    // Assert
    assert_eq!(
        error,
        ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitHub,
            message: "GitHub pull request was created but could not be reloaded".to_string(),
        }
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
    let status_context_error =
        GitHubReviewRequestAdapter::parse_status_check_rollup_pages(invalid_json)
            .err()
            .expect("invalid status-context JSON should fail");

    // Assert
    assert!(status_context_error.contains("invalid GitHub status-check contexts response"));
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
        GitHubReviewRequestAdapter::status_contexts_command(&remote, "42"),
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
