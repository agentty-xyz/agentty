//! `GitHub` pull-request merge tests: the recheck before merging, marking a
//! draft ready for review, merging, queueing, bypassing, and refusals,
//! against recorded `gh` output.

use std::sync::Arc;

use mockall::Sequence;

use super::support_test::{
    failure_output, github_remote, set_passing_required_checks, success_output,
};
use crate::client::ReviewRequestAdapter;
use crate::command::{ForgeCommand, ForgeCommandOutput, MockForgeCommandRunner};
use crate::github::{
    GitHubMergeMethod, GitHubMergeMethods, GitHubMergePlan, GitHubReviewRequestAdapter,
    PULL_REQUEST_MERGE_CANDIDATE_QUERY, PULL_REQUEST_MERGE_RESULT_QUERY, PULL_REQUEST_STATUS_QUERY,
    short_commit,
};
use crate::model::{
    ForgeKind, MergeReviewRequestInput, ReviewRequestError, ReviewRequestMergeMode,
    ReviewRequestMergeOutcome,
};

/// Head commit every recorded pre-merge response reports.
const HEAD_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// Merge commit the recorded post-merge response reports.
const MERGE_COMMIT: &str = "fedcba9876543210fedcba9876543210fedcba98";

/// `gh pr merge` output for a direct squash merge, written to stderr.
const SQUASHED_STDERR: &str =
    "✓ Squashed and merged pull request agentty-xyz/agentty#42 (Add forge review support)\n";

/// `gh pr merge` output for a pull request that joins a merge queue.
const QUEUED_STDERR: &str = "! The merge strategy for main is set by the merge queue\n✓ Pull \
                             request agentty-xyz/agentty#42 will be added to the merge queue for \
                             main when ready\n";

/// `gh pr merge` failure when the head moved after the recheck.
const HEAD_MOVED_STDERR: &str =
    "GraphQL: Head branch was modified. Review and try the merge again. (mergePullRequest)\n";

/// Returns a recorded pre-merge response for an open, clean, approved pull
/// request in a repository that allows every merge method.
fn ready_candidate() -> serde_json::Value {
    serde_json::json!({"data": {"repository": {
        "mergeCommitAllowed": true,
        "rebaseMergeAllowed": true,
        "squashMergeAllowed": true,
        "pullRequest": {
            "number": 42,
            "title": "Add forge review support",
            "state": "OPEN",
            "url": "https://github.com/agentty-xyz/agentty/pull/42",
            "baseRefName": "main",
            "headRefName": "feature/forge",
            "isDraft": false,
            "isInMergeQueue": false,
            "mergeable": "MERGEABLE",
            "mergeStateStatus": "CLEAN",
            "reviewDecision": "APPROVED",
            "mergedAt": null,
            "viewerCanMergeAsAdmin": true,
            "headRefOid": HEAD_COMMIT,
            "isMergeQueueEnabled": false,
            "autoMergeRequest": null
        }
    }}})
}

/// Returns a recorded pre-merge response for a published pull request that
/// only a missing approval blocks.
fn bypass_candidate() -> serde_json::Value {
    let mut candidate = ready_candidate();
    let pull_request = pull_request_mut(&mut candidate);
    pull_request["mergeStateStatus"] = serde_json::json!("BLOCKED");
    pull_request["reviewDecision"] = serde_json::json!("REVIEW_REQUIRED");
    set_passing_required_checks(pull_request);

    candidate
}

/// Returns a recorded pre-merge response for a conflict-free draft without
/// review threads, which merges only by bypass.
fn draft_bypass_candidate() -> serde_json::Value {
    let mut candidate = bypass_candidate();
    let pull_request = pull_request_mut(&mut candidate);
    pull_request["isDraft"] = serde_json::json!(true);
    pull_request["mergeStateStatus"] = serde_json::json!("DRAFT");
    pull_request["reviewThreads"] = serde_json::json!({"totalCount": 0, "nodes": []});

    candidate
}

/// Returns the pull-request node of a recorded pre-merge response.
fn pull_request_mut(candidate: &mut serde_json::Value) -> &mut serde_json::Value {
    &mut candidate["data"]["repository"]["pullRequest"]
}

/// Returns a recorded post-merge response.
fn merge_result(pull_request: &serde_json::Value) -> ForgeCommandOutput {
    success_output(
        serde_json::json!({"data": {"repository": {"pullRequest": pull_request}}}).to_string(),
    )
}

/// Returns the recorded post-merge response of a merged pull request.
fn merged_result() -> ForgeCommandOutput {
    merge_result(&serde_json::json!({
        "state": "MERGED",
        "mergedAt": "2026-11-27T10:00:00Z",
        "mergeCommit": {"oid": MERGE_COMMIT},
        "isInMergeQueue": false,
        "autoMergeRequest": null
    }))
}

/// Returns the recorded post-merge response of a pull request in the queue.
fn queued_result() -> ForgeCommandOutput {
    merge_result(&serde_json::json!({
        "state": "OPEN",
        "mergedAt": null,
        "mergeCommit": null,
        "isInMergeQueue": true,
        "autoMergeRequest": {"enabledAt": "2026-11-27T10:00:00Z"}
    }))
}

/// Returns one `gh pr merge` output that wrote `stderr` and succeeded.
fn merge_output(stderr: &str) -> ForgeCommandOutput {
    ForgeCommandOutput {
        exit_code: Some(0),
        stderr: stderr.to_string(),
        stdout: String::new(),
    }
}

/// Returns the merge input for pull request `#42` in `mode`, expecting the
/// recorded head commit.
fn merge_input(mode: ReviewRequestMergeMode) -> MergeReviewRequestInput {
    MergeReviewRequestInput {
        display_id: "#42".to_string(),
        expected_head_commit: Some(HEAD_COMMIT.to_string()),
        mode,
    }
}

/// Returns the pre-merge `gh api graphql` command for pull request `#42`.
fn candidate_command() -> ForgeCommand {
    GitHubReviewRequestAdapter::pull_request_graphql_command(
        &github_remote(),
        PULL_REQUEST_MERGE_CANDIDATE_QUERY,
        "42",
    )
}

/// Returns the post-merge `gh api graphql` command for pull request `#42`.
fn result_command() -> ForgeCommand {
    GitHubReviewRequestAdapter::pull_request_graphql_command(
        &github_remote(),
        PULL_REQUEST_MERGE_RESULT_QUERY,
        "42",
    )
}

/// Returns the `gh pr merge` arguments Agentty sends for pull request `#42`
/// with `flags` between the repository and the head-commit guard.
fn merge_arguments(flags: &[&str]) -> Vec<String> {
    let mut arguments = ["pr", "merge", "42", "--repo", "agentty-xyz/agentty"]
        .map(str::to_string)
        .to_vec();
    arguments.extend(flags.iter().map(|flag| (*flag).to_string()));
    arguments.extend(["--match-head-commit".to_string(), HEAD_COMMIT.to_string()]);

    arguments
}

/// Records the `gh` commands one merge runs, in order, each with its
/// recorded output.
struct RecordedGh {
    runner: MockForgeCommandRunner,
    sequence: Sequence,
}

impl RecordedGh {
    fn new() -> Self {
        Self {
            runner: MockForgeCommandRunner::new(),
            sequence: Sequence::new(),
        }
    }

    /// Expects `command` next and answers it with `output`.
    fn expect(mut self, command: ForgeCommand, output: ForgeCommandOutput) -> Self {
        self.runner
            .expect_run()
            .once()
            .in_sequence(&mut self.sequence)
            .withf(move |actual| actual == &command)
            .returning(move |_| {
                let output = output.clone();

                Box::pin(async move { Ok(output) })
            });

        self
    }

    /// Expects a `gh` command with exactly `arguments` next and answers it
    /// with `output`.
    fn expect_arguments(mut self, arguments: Vec<String>, output: ForgeCommandOutput) -> Self {
        self.runner
            .expect_run()
            .once()
            .in_sequence(&mut self.sequence)
            .withf(move |actual| actual.executable == "gh" && actual.arguments == arguments)
            .returning(move |_| {
                let output = output.clone();

                Box::pin(async move { Ok(output) })
            });

        self
    }

    /// Runs one merge of pull request `#42` against the recorded commands.
    async fn merge(
        self,
        input: MergeReviewRequestInput,
    ) -> Result<ReviewRequestMergeOutcome, ReviewRequestError> {
        GitHubReviewRequestAdapter::new(Arc::new(self.runner))
            .merge_authenticated_review_request(github_remote(), input)
            .await
    }
}

#[tokio::test]
async fn ready_pull_request_squash_merges_and_reports_the_merge_commit() {
    // Arrange
    let gh = RecordedGh::new()
        .expect(
            candidate_command(),
            success_output(ready_candidate().to_string()),
        )
        .expect_arguments(
            merge_arguments(&["--squash"]),
            merge_output(SQUASHED_STDERR),
        )
        .expect(result_command(), merged_result());

    // Act
    let outcome = gh.merge(merge_input(ReviewRequestMergeMode::Ready)).await;

    // Assert
    assert_eq!(
        outcome,
        Ok(ReviewRequestMergeOutcome::Merged {
            merge_commit: Some(MERGE_COMMIT.to_string()),
        })
    );
}

#[tokio::test]
async fn ready_pull_request_joins_a_required_merge_queue_without_a_method() {
    // Arrange
    let mut candidate = ready_candidate();
    pull_request_mut(&mut candidate)["isMergeQueueEnabled"] = serde_json::json!(true);
    let gh = RecordedGh::new()
        .expect(candidate_command(), success_output(candidate.to_string()))
        .expect_arguments(merge_arguments(&[]), merge_output(QUEUED_STDERR))
        .expect(result_command(), queued_result());

    // Act
    let outcome = gh.merge(merge_input(ReviewRequestMergeMode::Ready)).await;

    // Assert
    assert_eq!(outcome, Ok(ReviewRequestMergeOutcome::Queued));
}

#[tokio::test]
async fn queued_merge_set_to_join_the_queue_later_counts_as_queued() {
    // Arrange
    let mut candidate = ready_candidate();
    pull_request_mut(&mut candidate)["isMergeQueueEnabled"] = serde_json::json!(true);
    let auto_merge = merge_result(&serde_json::json!({
        "state": "OPEN",
        "mergedAt": null,
        "mergeCommit": null,
        "isInMergeQueue": false,
        "autoMergeRequest": {"enabledAt": "2026-11-27T10:00:00Z"}
    }));
    let gh = RecordedGh::new()
        .expect(candidate_command(), success_output(candidate.to_string()))
        .expect_arguments(merge_arguments(&[]), merge_output(QUEUED_STDERR))
        .expect(result_command(), auto_merge);

    // Act
    let outcome = gh.merge(merge_input(ReviewRequestMergeMode::Ready)).await;

    // Assert
    assert_eq!(outcome, Ok(ReviewRequestMergeOutcome::Queued));
}

#[tokio::test]
async fn bypass_merges_with_administrator_rights_past_a_merge_queue() {
    // Arrange
    let mut candidate = bypass_candidate();
    pull_request_mut(&mut candidate)["isMergeQueueEnabled"] = serde_json::json!(true);
    let gh = RecordedGh::new()
        .expect(candidate_command(), success_output(candidate.to_string()))
        .expect_arguments(
            merge_arguments(&["--squash", "--admin"]),
            merge_output(SQUASHED_STDERR),
        )
        .expect(result_command(), merged_result());

    // Act
    let outcome = gh.merge(merge_input(ReviewRequestMergeMode::Bypass)).await;

    // Assert
    assert_eq!(
        outcome,
        Ok(ReviewRequestMergeOutcome::Merged {
            merge_commit: Some(MERGE_COMMIT.to_string()),
        })
    );
}

#[tokio::test]
async fn draft_is_marked_ready_for_review_before_its_bypass_merge() {
    // Arrange
    let merged_without_commit = merge_result(&serde_json::json!({
        "state": "MERGED",
        "mergedAt": null,
        "mergeCommit": null,
        "isInMergeQueue": false,
        "autoMergeRequest": null
    }));
    let gh = RecordedGh::new()
        .expect(
            candidate_command(),
            success_output(draft_bypass_candidate().to_string()),
        )
        .expect_arguments(
            ["pr", "ready", "42", "--repo", "agentty-xyz/agentty"]
                .map(str::to_string)
                .to_vec(),
            merge_output(
                "✓ Pull request agentty-xyz/agentty#42 is marked as \"ready for review\"\n",
            ),
        )
        .expect_arguments(
            merge_arguments(&["--squash", "--admin"]),
            merge_output(SQUASHED_STDERR),
        )
        .expect(result_command(), merged_without_commit);

    // Act
    let outcome = gh.merge(merge_input(ReviewRequestMergeMode::Bypass)).await;

    // Assert
    assert_eq!(
        outcome,
        Ok(ReviewRequestMergeOutcome::Merged { merge_commit: None })
    );
}

#[tokio::test]
async fn merge_failure_after_marking_a_draft_ready_says_it_stays_ready() {
    // Arrange
    let gh = RecordedGh::new()
        .expect(
            candidate_command(),
            success_output(draft_bypass_candidate().to_string()),
        )
        .expect_arguments(
            ["pr", "ready", "42", "--repo", "agentty-xyz/agentty"]
                .map(str::to_string)
                .to_vec(),
            merge_output(""),
        )
        .expect_arguments(
            merge_arguments(&["--squash", "--admin"]),
            failure_output(HEAD_MOVED_STDERR.to_string()),
        );

    // Act
    let error = gh
        .merge(merge_input(ReviewRequestMergeMode::Bypass))
        .await
        .expect_err("a failed merge should fail");

    // Assert
    assert_eq!(
        error,
        ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitHub,
            message: "merge pull request: GraphQL: Head branch was modified. Review and try the \
                      merge again. (mergePullRequest)\nPull request #42 was marked ready for \
                      review and stays ready for review."
                .to_string(),
        }
    );
}

#[tokio::test]
async fn merge_failure_of_a_published_pull_request_reports_only_the_failure() {
    // Arrange
    let gh = RecordedGh::new()
        .expect(
            candidate_command(),
            success_output(ready_candidate().to_string()),
        )
        .expect_arguments(
            merge_arguments(&["--squash"]),
            failure_output(HEAD_MOVED_STDERR.to_string()),
        );

    // Act
    let error = gh
        .merge(merge_input(ReviewRequestMergeMode::Ready))
        .await
        .expect_err("a failed merge should fail");

    // Assert
    assert!(matches!(
        error,
        ReviewRequestError::OperationFailed { ref message, .. }
            if message.starts_with("merge pull request: GraphQL: Head branch was modified")
                && !message.contains("ready for review")
    ));
}

#[tokio::test]
async fn failed_ready_for_review_stops_before_merging() {
    // Arrange
    let gh = RecordedGh::new()
        .expect(
            candidate_command(),
            success_output(draft_bypass_candidate().to_string()),
        )
        .expect_arguments(
            ["pr", "ready", "42", "--repo", "agentty-xyz/agentty"]
                .map(str::to_string)
                .to_vec(),
            failure_output("GraphQL: Resource not accessible by integration\n".to_string()),
        );

    // Act
    let error = gh
        .merge(merge_input(ReviewRequestMergeMode::Bypass))
        .await
        .expect_err("a failed ready-for-review should fail");

    // Assert
    assert_eq!(
        error,
        ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitHub,
            message: "mark pull request ready for review: GraphQL: Resource not accessible by \
                      integration"
                .to_string(),
        }
    );
}

#[tokio::test]
async fn merge_that_neither_merged_nor_queued_fails() {
    // Arrange
    let still_open = merge_result(&serde_json::json!({
        "state": "OPEN",
        "mergedAt": null,
        "mergeCommit": null,
        "isInMergeQueue": false,
        "autoMergeRequest": null
    }));
    let gh = RecordedGh::new()
        .expect(
            candidate_command(),
            success_output(ready_candidate().to_string()),
        )
        .expect_arguments(merge_arguments(&["--squash"]), merge_output(""))
        .expect(result_command(), still_open);

    // Act
    let error = gh
        .merge(merge_input(ReviewRequestMergeMode::Ready))
        .await
        .expect_err("a merge with no effect should fail");

    // Assert
    assert_eq!(
        error,
        ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitHub,
            message: "GitHub accepted the merge, but pull request #42 is neither merged nor queued"
                .to_string(),
        }
    );
}

#[tokio::test]
async fn unreadable_merge_result_fails() {
    // Arrange
    let gh = RecordedGh::new()
        .expect(
            candidate_command(),
            success_output(ready_candidate().to_string()),
        )
        .expect_arguments(merge_arguments(&["--squash"]), merge_output(""))
        .expect(
            result_command(),
            success_output(r#"{"data":{"repository":{"pullRequest":null}}}"#.to_string()),
        );

    // Act
    let error = gh
        .merge(merge_input(ReviewRequestMergeMode::Ready))
        .await
        .expect_err("a merge result without a pull request should fail");
    let invalid = GitHubReviewRequestAdapter::parse_merge_result("{")
        .err()
        .unwrap_or_default();

    // Assert
    assert!(error.detail_message().contains("missing a pull request"));
    assert!(invalid.starts_with("invalid GitHub pull-request merge result"));
}

#[tokio::test]
async fn recheck_that_no_longer_allows_the_merge_refuses_without_changes() {
    // Arrange
    let mut candidate = ready_candidate();
    pull_request_mut(&mut candidate)["mergeStateStatus"] = serde_json::json!("UNSTABLE");
    let gh = RecordedGh::new().expect(candidate_command(), success_output(candidate.to_string()));

    // Act
    let outcome = gh.merge(merge_input(ReviewRequestMergeMode::Ready)).await;

    // Assert
    assert_eq!(
        outcome,
        Ok(ReviewRequestMergeOutcome::Refused {
            reason: "Pull request #42 is no longer ready to merge (Approved, Checks pending)"
                .to_string(),
        })
    );
}

#[tokio::test]
async fn recheck_loads_every_check_page_before_a_bypass() {
    // Arrange
    let mut candidate = bypass_candidate();
    let pull_request = pull_request_mut(&mut candidate);
    pull_request["statusCheckRollup"] = serde_json::json!({
        "state": "SUCCESS",
        "contexts": {"nodes": [{"context": "codecov/patch"}], "pageInfo": {"hasNextPage": true}}
    });
    let all_pages = serde_json::json!([{"data": {"repository": {"pullRequest": {
        "statusCheckRollup": {
            "state": "SUCCESS",
            "contexts": {
                "nodes": [{"context": "codecov/patch"}, {"name": "Workspace validation"}],
                "pageInfo": {"hasNextPage": false, "endCursor": null}
            }
        }
    }}}}]);
    let gh = RecordedGh::new()
        .expect(candidate_command(), success_output(candidate.to_string()))
        .expect(
            GitHubReviewRequestAdapter::status_contexts_command(&github_remote(), "42"),
            success_output(all_pages.to_string()),
        )
        .expect_arguments(
            merge_arguments(&["--squash", "--admin"]),
            merge_output(SQUASHED_STDERR),
        )
        .expect(result_command(), merged_result());

    // Act
    let outcome = gh.merge(merge_input(ReviewRequestMergeMode::Bypass)).await;

    // Assert
    assert!(matches!(
        outcome,
        Ok(ReviewRequestMergeOutcome::Merged { .. })
    ));
}

#[tokio::test]
async fn recheck_failure_and_malformed_responses_fail_before_merging() {
    // Arrange
    let failed = RecordedGh::new().expect(
        candidate_command(),
        failure_output("GraphQL: Could not resolve to a PullRequest\n".to_string()),
    );
    let no_repository = RecordedGh::new().expect(
        candidate_command(),
        success_output(r#"{"data":{"repository":null}}"#.to_string()),
    );
    let no_pull_request = RecordedGh::new().expect(
        candidate_command(),
        success_output(r#"{"data":{"repository":{"pullRequest":null}}}"#.to_string()),
    );
    let invalid = RecordedGh::new().expect(candidate_command(), success_output("{".to_string()));

    // Act
    let errors = [
        failed
            .merge(merge_input(ReviewRequestMergeMode::Ready))
            .await,
        no_repository
            .merge(merge_input(ReviewRequestMergeMode::Ready))
            .await,
        no_pull_request
            .merge(merge_input(ReviewRequestMergeMode::Ready))
            .await,
        invalid
            .merge(merge_input(ReviewRequestMergeMode::Ready))
            .await,
    ]
    .map(|result| {
        result
            .expect_err("the recheck should fail")
            .detail_message()
    });

    // Assert
    assert!(errors[0].contains("read pull request before merging"));
    assert!(errors[1].contains("missing a repository"));
    assert!(errors[2].contains("missing a pull request"));
    assert!(errors[3].contains("invalid GitHub pull-request merge response"));
}

#[tokio::test]
async fn invalid_display_id_fails_without_running_gh() {
    // Arrange
    let gh = RecordedGh::new();
    let input = MergeReviewRequestInput {
        display_id: "!42".to_string(),
        ..merge_input(ReviewRequestMergeMode::Ready)
    };

    // Act
    let error = gh.merge(input).await.expect_err("invalid id should fail");

    // Assert
    assert!(
        error
            .detail_message()
            .contains("invalid GitHub pull-request display id")
    );
}

/// One change to a ready pull request, the merge mode asked for, and the
/// refusal the recheck must give.
type RefusalCase = (
    fn(&mut serde_json::Value),
    ReviewRequestMergeMode,
    &'static str,
);

#[test]
fn merge_plan_refuses_a_pull_request_that_changed_since_its_refresh() {
    // Arrange
    let cases: [RefusalCase; 8] = [
        (
            |pull_request| pull_request["state"] = serde_json::json!("MERGED"),
            ReviewRequestMergeMode::Ready,
            "Pull request #42 is already merged",
        ),
        (
            |pull_request| pull_request["state"] = serde_json::json!("CLOSED"),
            ReviewRequestMergeMode::Ready,
            "Pull request #42 is closed",
        ),
        (
            |pull_request| pull_request["isInMergeQueue"] = serde_json::json!(true),
            ReviewRequestMergeMode::Ready,
            "Pull request #42 is already in the merge queue",
        ),
        (
            |pull_request| {
                pull_request["autoMergeRequest"] = serde_json::json!({"enabledAt": "2026"});
            },
            ReviewRequestMergeMode::Ready,
            "Pull request #42 is already set to merge automatically",
        ),
        (
            |pull_request| pull_request["headRefOid"] = serde_json::json!("9999999aaaa"),
            ReviewRequestMergeMode::Ready,
            "Pull request #42 is at commit `9999999`, not `0123456`; push or sync the branch, \
             then merge again",
        ),
        (
            |_| {},
            ReviewRequestMergeMode::Bypass,
            "Pull request #42 is now ready to merge without bypassing branch protections; merge \
             it without bypass",
        ),
        (
            |pull_request| {
                pull_request["mergeStateStatus"] = serde_json::Value::Null;
                pull_request["reviewDecision"] = serde_json::Value::Null;
            },
            ReviewRequestMergeMode::Bypass,
            "Pull request #42 is no longer ready to merge with bypass (no status)",
        ),
        (
            |pull_request| pull_request["mergeStateStatus"] = serde_json::json!("BLOCKED"),
            ReviewRequestMergeMode::Ready,
            "Pull request #42 is no longer ready to merge (Approved, Blocked)",
        ),
    ];

    // Act
    let refusals = cases.map(|(change, mode, _)| {
        let mut candidate = ready_candidate();
        change(pull_request_mut(&mut candidate));
        let (candidate, merge_methods) =
            GitHubReviewRequestAdapter::parse_merge_candidate(&candidate.to_string())
                .expect("candidate should parse");

        candidate
            .merge_plan(&merge_methods, &merge_input(mode))
            .expect_err("changed pull request should be refused")
    });

    // Assert
    assert_eq!(refusals, cases.map(|(_, _, reason)| reason.to_string()));
}

#[test]
fn merge_plan_without_an_expected_head_accepts_the_reported_head() {
    // Arrange
    let (candidate, merge_methods) =
        GitHubReviewRequestAdapter::parse_merge_candidate(&ready_candidate().to_string())
            .expect("candidate should parse");
    let input = MergeReviewRequestInput {
        expected_head_commit: None,
        ..merge_input(ReviewRequestMergeMode::Ready)
    };

    // Act
    let plan = candidate.merge_plan(&merge_methods, &input);

    // Assert
    assert_eq!(
        plan,
        Ok(GitHubMergePlan {
            bypasses_protections: false,
            head_commit: HEAD_COMMIT.to_string(),
            method: Some(GitHubMergeMethod::Squash),
        })
    );
}

#[test]
fn merge_plan_refuses_a_repository_without_merge_methods() {
    // Arrange
    let mut response = ready_candidate();
    for method in [
        "mergeCommitAllowed",
        "rebaseMergeAllowed",
        "squashMergeAllowed",
    ] {
        response["data"]["repository"][method] = serde_json::json!(false);
    }
    let (candidate, merge_methods) =
        GitHubReviewRequestAdapter::parse_merge_candidate(&response.to_string())
            .expect("candidate should parse");

    // Act
    let refusal = candidate.merge_plan(&merge_methods, &merge_input(ReviewRequestMergeMode::Ready));

    // Assert
    assert_eq!(
        refusal,
        Err("The repository of pull request #42 allows no merge method".to_string())
    );
}

#[test]
fn preferred_merge_method_is_squash_then_merge_commit_then_rebase() {
    // Arrange
    let methods =
        |merge_commit_allowed, rebase_merge_allowed, squash_merge_allowed| GitHubMergeMethods {
            merge_commit_allowed,
            rebase_merge_allowed,
            squash_merge_allowed,
        };

    // Act
    let preferred = [
        methods(true, true, true).preferred(),
        methods(true, true, false).preferred(),
        methods(false, true, false).preferred(),
        methods(false, false, false).preferred(),
    ];

    // Assert
    assert_eq!(
        preferred,
        [
            Some(GitHubMergeMethod::Squash),
            Some(GitHubMergeMethod::Merge),
            Some(GitHubMergeMethod::Rebase),
            None,
        ]
    );
    assert_eq!(
        [
            GitHubMergeMethod::Squash,
            GitHubMergeMethod::Merge,
            GitHubMergeMethod::Rebase
        ]
        .map(GitHubMergeMethod::flag),
        ["--squash", "--merge", "--rebase"]
    );
}

#[test]
fn stays_ready_error_keeps_the_detail_of_any_failure() {
    // Arrange
    let authentication = ReviewRequestError::AuthenticationRequired {
        detail: None,
        forge_kind: ForgeKind::GitHub,
        host: "github.com".to_string(),
    };

    // Act
    let error = GitHubReviewRequestAdapter::stays_ready_for_review_error(&authentication, "42");

    // Assert
    assert_eq!(
        error,
        ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitHub,
            message: format!(
                "{}\nPull request #42 was marked ready for review and stays ready for review.",
                authentication.detail_message()
            ),
        }
    );
}

#[test]
fn merge_queries_extend_the_refresh_status_fields() {
    // Arrange
    let status_fields = PULL_REQUEST_STATUS_QUERY
        .split_once("pullRequest(number: $number) { ")
        .and_then(|(_, rest)| rest.strip_suffix(" } } }"))
        .expect("status query should select pull-request fields");

    // Act
    let candidate_fields = PULL_REQUEST_MERGE_CANDIDATE_QUERY
        .split_once("pullRequest(number: $number) { ")
        .map(|(_, rest)| rest)
        .expect("candidate query should select pull-request fields");

    // Assert
    assert!(status_fields.ends_with("viewerCanMergeAsAdmin"));
    assert_eq!(
        candidate_fields,
        format!(
            "{status_fields} headRefOid isMergeQueueEnabled autoMergeRequest {{ enabledAt }} }} \
             }} }}"
        )
    );
    assert!(PULL_REQUEST_MERGE_CANDIDATE_QUERY.contains(
        "repository(owner: $owner, name: $repo) { mergeCommitAllowed rebaseMergeAllowed \
         squashMergeAllowed pullRequest"
    ));
    assert!(PULL_REQUEST_MERGE_RESULT_QUERY.contains(
        "pullRequest(number: $number) { state mergedAt mergeCommit { oid } isInMergeQueue \
         autoMergeRequest { enabledAt } }"
    ));
}

#[test]
fn short_commit_abbreviates_only_long_hashes() {
    // Arrange
    let commits = [HEAD_COMMIT, "abc"];

    // Act
    let short = commits.map(short_commit);

    // Assert
    assert_eq!(short, ["0123456", "abc"]);
}
