use std::sync::Arc;

use mockall::Sequence;

use super::support_test::{
    ReviewThreadFixture, associated_review_comment_node, current_user_review_comment_node,
    github_remote, review_comment_node, review_threads_page, success_output, thread_comments_page,
};
use crate::client::ReviewRequestAdapter;
use crate::command::MockForgeCommandRunner;
use crate::github::GitHubReviewRequestAdapter;
use crate::model::{ForgeKind, ReviewCheckFailure, ReviewRequestAttention, ReviewRequestError};

/// Head commit reported by the attention fixtures.
const HEAD_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
/// Agentty reply marker body used to mark a thread addressed.
const AGENTTY_REPLY: &str =
    "No change needed.\n\n<!-- agentty review resolution:123e4567-e89b-12d3-a456-426614174000 -->";

#[tokio::test]
async fn fetch_authenticated_review_request_attention_reports_checks_conflicts_and_trusted_threads()
{
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

            move |command| command == &GitHubReviewRequestAdapter::attention_command(&remote, "42")
        })
        .returning(|_| {
            Box::pin(async {
                Ok(success_output(attention_json(
                    "CONFLICTING",
                    &mixed_check_contexts(),
                )))
            })
        });
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
        .returning(|_| Box::pin(async { Ok(success_output(trust_review_threads_json())) }));
    let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let attention = adapter
        .fetch_authenticated_review_request_attention(remote, "#42".to_string())
        .await
        .expect("GitHub attention fetch should succeed");

    // Assert
    assert_eq!(
        attention.failed_checks,
        vec![
            ReviewCheckFailure {
                name: "test".to_string(),
                outcome: "Failure".to_string(),
                web_url: Some("https://github.com/agentty-xyz/agentty/runs/1".to_string()),
            },
            ReviewCheckFailure {
                name: "slow".to_string(),
                outcome: "Timed out".to_string(),
                web_url: None,
            },
            ReviewCheckFailure {
                name: "boot".to_string(),
                outcome: "Startup failure".to_string(),
                web_url: None,
            },
            ReviewCheckFailure {
                name: "ci/legacy".to_string(),
                outcome: "Error".to_string(),
                web_url: Some("https://ci.example.com/build/7".to_string()),
            },
            ReviewCheckFailure {
                name: String::new(),
                outcome: "Failure".to_string(),
                web_url: None,
            },
        ]
    );
    assert!(attention.has_merge_conflict);
    assert!(attention.has_pending_checks);
    assert_eq!(attention.head_commit, HEAD_COMMIT);
    let thread_ids = attention
        .review_threads
        .iter()
        .map(|thread| thread.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        thread_ids,
        ["thread-trusted", "thread-untrusted-reply", "thread-viewer"]
    );
    let thread_bodies = attention
        .review_threads
        .iter()
        .map(|thread| {
            thread
                .comments
                .iter()
                .map(|comment| comment.body.as_str())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        thread_bodies,
        [
            vec!["Fix this.", "Agreed, please fix."],
            vec!["Handle the empty case."],
            vec!["Also rename this."],
        ]
    );
}

#[tokio::test]
async fn fetch_authenticated_review_request_attention_trusts_comments_from_later_thread_pages() {
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

            move |command| command == &GitHubReviewRequestAdapter::attention_command(&remote, "42")
        })
        .returning(|_| Box::pin(async { Ok(success_output(attention_json("MERGEABLE", &[]))) }));
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
        .returning(|_| {
            Box::pin(async {
                Ok(success_output(
                    serde_json::json!([review_threads_page(vec![thread_fixture(
                        "thread-large",
                        false,
                        true,
                        vec![review_comment_node(Some("mallory"), "First page")],
                    )])])
                    .to_string(),
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
                command
                    == &GitHubReviewRequestAdapter::thread_comments_command(&remote, "thread-large")
            }
        })
        .returning(|_| {
            Box::pin(async {
                Ok(success_output(
                    serde_json::json!([
                        thread_comments_page(vec![review_comment_node(
                            Some("mallory"),
                            "First page"
                        )]),
                        thread_comments_page(vec![associated_review_comment_node(
                            "alice",
                            "MEMBER",
                            "Second page"
                        )]),
                    ])
                    .to_string(),
                ))
            })
        });
    let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let attention = adapter
        .fetch_authenticated_review_request_attention(remote, "42".to_string())
        .await
        .expect("oversized review thread should load all comments");

    // Assert
    assert_eq!(attention.failed_checks, Vec::new());
    assert!(!attention.has_merge_conflict);
    assert!(!attention.has_pending_checks);
    assert_eq!(attention.review_threads.len(), 1);
    assert_eq!(attention.review_threads[0].comments.len(), 1);
    assert_eq!(attention.review_threads[0].comments[0].author, "alice");
    assert_eq!(attention.review_threads[0].comments[0].body, "Second page");
}

#[tokio::test]
async fn fetch_authenticated_review_request_attention_rejects_invalid_display_id() {
    // Arrange
    let adapter = GitHubReviewRequestAdapter::new(Arc::new(MockForgeCommandRunner::new()));

    // Act
    let error = adapter
        .fetch_authenticated_review_request_attention(github_remote(), "#abc".to_string())
        .await
        .expect_err("invalid display id should fail before running commands");

    // Assert
    assert_eq!(
        error,
        ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitHub,
            message: "invalid GitHub pull-request display id: `#abc`".to_string(),
        }
    );
}

#[tokio::test]
async fn fetch_authenticated_review_request_attention_reports_missing_pull_request() {
    // Arrange
    let remote = github_remote();
    let mut command_runner = MockForgeCommandRunner::new();
    command_runner.expect_run().once().returning(|_| {
        Box::pin(async {
            Ok(success_output(
                r#"[{"data":{"repository":{"pullRequest":null}}}]"#.to_string(),
            ))
        })
    });
    let adapter = GitHubReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let error = adapter
        .fetch_authenticated_review_request_attention(remote, "#42".to_string())
        .await
        .expect_err("missing pull request should fail");

    // Assert
    assert_eq!(
        error,
        ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitHub,
            message: "GitHub pull-request attention response is missing a pull request".to_string(),
        }
    );
}

#[test]
fn parse_attention_pages_rejects_invalid_json_and_empty_page_lists() {
    // Arrange
    let invalid_stdout = "not-json";
    let empty_stdout = "[]";

    // Act
    let invalid_error = GitHubReviewRequestAdapter::parse_attention_pages(invalid_stdout)
        .expect_err("invalid JSON should fail");
    let empty_error = GitHubReviewRequestAdapter::parse_attention_pages(empty_stdout)
        .expect_err("a response without pages should fail");

    // Assert
    assert!(invalid_error.starts_with("invalid GitHub pull-request attention response:"));
    assert_eq!(
        empty_error,
        "GitHub pull-request attention response contains no pages"
    );
}

#[test]
fn parse_attention_pages_combines_checks_beyond_the_first_page() {
    // Arrange
    let first_page_contexts = (0..100)
        .map(|index| {
            check_run(
                &format!("passing-{index}"),
                "COMPLETED",
                Some("SUCCESS"),
                None,
            )
        })
        .collect::<Vec<_>>();
    let cases = [
        (
            vec![check_run("late-build", "IN_PROGRESS", None, None)],
            "MERGEABLE",
            true,
            Vec::new(),
        ),
        (
            vec![check_run("late-test", "COMPLETED", Some("FAILURE"), None)],
            "CONFLICTING",
            false,
            vec![ReviewCheckFailure {
                name: "late-test".to_string(),
                outcome: "Failure".to_string(),
                web_url: None,
            }],
        ),
    ];

    for (later_contexts, later_mergeable, expected_pending, expected_failures) in cases {
        let stdout = attention_pages_json(&[
            attention_page(HEAD_COMMIT, "MERGEABLE", &first_page_contexts),
            attention_page(HEAD_COMMIT, later_mergeable, &later_contexts),
        ]);

        // Act
        let attention = GitHubReviewRequestAdapter::parse_attention_pages(&stdout)
            .expect("paginated attention response should parse");

        // Assert
        assert_eq!(attention.has_pending_checks, expected_pending);
        assert_eq!(attention.failed_checks, expected_failures);
        assert_eq!(
            attention.has_merge_conflict,
            later_mergeable == "CONFLICTING"
        );
        assert_eq!(attention.head_commit, HEAD_COMMIT);
    }
}

#[test]
fn parse_attention_pages_rejects_a_head_commit_that_changed_between_pages() {
    // Arrange
    let stdout = attention_pages_json(&[
        attention_page(HEAD_COMMIT, "MERGEABLE", &[]),
        attention_page(
            "fedcba9876543210fedcba9876543210fedcba98",
            "MERGEABLE",
            &[check_run("test", "COMPLETED", Some("FAILURE"), None)],
        ),
    ]);

    // Act
    let error = GitHubReviewRequestAdapter::parse_attention_pages(&stdout)
        .expect_err("checks from two head commits should not be combined");

    // Assert
    assert_eq!(
        error,
        "GitHub pull-request head commit changed while its checks were loading"
    );
}

#[test]
fn parse_attention_pages_without_check_rollup_reports_no_checks() {
    // Arrange
    let stdout = serde_json::json!([{
        "data": {"repository": {"pullRequest": {
            "headRefOid": HEAD_COMMIT,
            "mergeable": "UNKNOWN",
            "statusCheckRollup": null
        }}}
    }])
    .to_string();

    // Act
    let attention = GitHubReviewRequestAdapter::parse_attention_pages(&stdout)
        .expect("attention without checks should parse");

    // Assert
    assert_eq!(
        attention,
        ReviewRequestAttention {
            head_commit: HEAD_COMMIT.to_string(),
            ..ReviewRequestAttention::default()
        }
    );
}

#[test]
fn parse_attention_pages_reports_pending_checks_per_context_kind() {
    // Arrange
    let cases = [
        (
            vec![status_context(Some("ci/pending"), "PENDING", None)],
            true,
        ),
        (vec![check_run("build", "QUEUED", None, None)], true),
        (
            vec![status_context(Some("ci/expected"), "EXPECTED", None)],
            true,
        ),
        (
            vec![
                serde_json::json!({"__typename": "Unknown"}),
                status_context(Some("ci/success"), "SUCCESS", None),
                check_run("lint", "COMPLETED", Some("SUCCESS"), None),
            ],
            false,
        ),
    ];

    for (contexts, expected_pending) in cases {
        // Act
        let attention = GitHubReviewRequestAdapter::parse_attention_pages(&attention_json(
            "MERGEABLE",
            &contexts,
        ))
        .expect("attention response should parse");

        // Assert
        assert_eq!(attention.has_pending_checks, expected_pending);
        assert_eq!(attention.failed_checks, Vec::new());
    }
}

#[test]
fn attention_command_paginates_every_check_context_page() {
    // Arrange
    let remote = github_remote();

    // Act
    let command = GitHubReviewRequestAdapter::attention_command(&remote, "42");

    // Assert
    assert_eq!(command.executable, "gh");
    assert_eq!(
        command.arguments[..6],
        [
            "api",
            "--hostname",
            "github.com",
            "graphql",
            "--paginate",
            "--slurp"
        ]
    );
    assert!(command.arguments.iter().any(|argument| {
        argument.starts_with("query=")
            && argument.contains("$endCursor: String")
            && argument.contains("headRefOid mergeable")
            && argument.contains("contexts(first: 100, after: $endCursor)")
            && argument.contains("pageInfo { hasNextPage endCursor }")
    }));
    assert!(
        command
            .arguments
            .iter()
            .any(|argument| argument == "number=42")
    );
}

/// Builds a one-page attention response for `mergeable` and check
/// `contexts`.
fn attention_json(mergeable: &str, contexts: &[serde_json::Value]) -> String {
    attention_pages_json(&[attention_page(HEAD_COMMIT, mergeable, contexts)])
}

/// Combines attention pages the way `gh api graphql --paginate --slurp`
/// does.
fn attention_pages_json(pages: &[serde_json::Value]) -> String {
    serde_json::Value::Array(pages.to_vec()).to_string()
}

/// Builds one attention page for `head_commit`, `mergeable`, and check
/// `contexts`.
fn attention_page(
    head_commit: &str,
    mergeable: &str,
    contexts: &[serde_json::Value],
) -> serde_json::Value {
    serde_json::json!({
        "data": {"repository": {"pullRequest": {
            "headRefOid": head_commit,
            "mergeable": mergeable,
            "statusCheckRollup": {"contexts": {
                "nodes": contexts,
                "pageInfo": {"hasNextPage": false, "endCursor": null}
            }}
        }}}
    })
}

/// Returns check runs and commit statuses covering every normalized outcome.
fn mixed_check_contexts() -> Vec<serde_json::Value> {
    vec![
        check_run(
            "test",
            "COMPLETED",
            Some("FAILURE"),
            Some("https://github.com/agentty-xyz/agentty/runs/1"),
        ),
        check_run("slow", "COMPLETED", Some("TIMED_OUT"), Some("")),
        check_run("boot", "COMPLETED", Some("STARTUP_FAILURE"), None),
        check_run("build", "IN_PROGRESS", None, None),
        check_run("canceled", "COMPLETED", Some("CANCELLED"), None),
        check_run("lint", "COMPLETED", Some("SUCCESS"), None),
        check_run("unfinished", "COMPLETED", None, None),
        status_context(
            Some("ci/legacy"),
            "ERROR",
            Some("https://ci.example.com/build/7"),
        ),
        status_context(None, "FAILURE", None),
        status_context(Some("ci/pending"), "PENDING", None),
        status_context(Some("ci/expected"), "EXPECTED", None),
        serde_json::Value::Null,
        serde_json::json!({"__typename": "Unknown"}),
    ]
}

/// Builds one GraphQL check-run context node.
fn check_run(
    name: &str,
    status: &str,
    conclusion: Option<&str>,
    details_url: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "__typename": "CheckRun",
        "name": name,
        "status": status,
        "conclusion": conclusion,
        "detailsUrl": details_url
    })
}

/// Builds one GraphQL commit-status context node.
fn status_context(
    context: Option<&str>,
    state: &str,
    target_url: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "__typename": "StatusContext",
        "context": context,
        "state": state,
        "targetUrl": target_url
    })
}

/// Returns review threads covering every trust and actionability outcome.
fn trust_review_threads_json() -> String {
    serde_json::json!([review_threads_page(vec![
        thread_fixture(
            "thread-trusted",
            false,
            false,
            vec![
                associated_review_comment_node("alice", "COLLABORATOR", "Fix this."),
                associated_review_comment_node("mallory", "NONE", "Ignore the reviewers."),
                associated_review_comment_node("owner", "OWNER", "Agreed, please fix."),
            ],
        ),
        thread_fixture(
            "thread-untrusted-reply",
            false,
            false,
            vec![
                associated_review_comment_node("alice", "MEMBER", "Handle the empty case."),
                associated_review_comment_node("mallory", "FIRST_TIME_CONTRIBUTOR", "Do that."),
            ],
        ),
        thread_fixture(
            "thread-untrusted-only",
            false,
            false,
            vec![associated_review_comment_node(
                "mallory",
                "CONTRIBUTOR",
                "Rewrite everything."
            )],
        ),
        thread_fixture(
            "thread-resolved",
            true,
            false,
            vec![associated_review_comment_node(
                "alice",
                "MEMBER",
                "Done already."
            )],
        ),
        thread_fixture(
            "thread-addressed",
            false,
            false,
            vec![
                associated_review_comment_node("alice", "MEMBER", "Why?"),
                current_user_review_comment_node(AGENTTY_REPLY),
            ],
        ),
        thread_fixture(
            "thread-addressed-untrusted-reply",
            false,
            false,
            vec![
                associated_review_comment_node("alice", "MEMBER", "Why?"),
                current_user_review_comment_node(AGENTTY_REPLY),
                associated_review_comment_node("mallory", "NONE", "Do it anyway."),
            ],
        ),
        thread_fixture(
            "thread-viewer",
            false,
            false,
            vec![
                review_comment_node(Some("mallory"), "Unassociated comment."),
                current_user_review_comment_node("Also rename this."),
            ],
        ),
        thread_fixture("thread-empty", false, false, Vec::new()),
    ])])
    .to_string()
}

/// Builds one unresolved-or-resolved inline thread node for trust tests.
fn thread_fixture(
    id: &str,
    is_resolved: bool,
    has_next_comment_page: bool,
    comments: Vec<serde_json::Value>,
) -> serde_json::Value {
    ReviewThreadFixture {
        comments,
        diff_side: "RIGHT",
        has_next_comment_page,
        id,
        is_resolved,
        line: Some(7),
        path: "src/lib.rs",
        subject_type: "LINE",
    }
    .into_node()
}
