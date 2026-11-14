use std::sync::Arc;

use mockall::Sequence;

use super::support_test::{gitlab_current_user_json, gitlab_remote, success_output};
use crate::client::ReviewRequestAdapter;
use crate::command::{ForgeCommand, ForgeCommandOutput, MockForgeCommandRunner};
use crate::gitlab::{GitLabPipeline, GitLabReviewRequestAdapter};
use crate::model::{ForgeKind, ReviewCheckFailure, ReviewRequestAttention, ReviewRequestError};

/// Head commit reported by the attention fixtures.
const HEAD_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
/// Agentty reply marker body used to mark a discussion addressed.
const AGENTTY_REPLY: &str =
    "No change needed.\n\n<!-- agentty review resolution:123e4567-e89b-12d3-a456-426614174000 -->";

#[tokio::test]
async fn fetch_authenticated_review_request_attention_reports_failed_jobs_and_trusted_threads() {
    // Arrange
    let remote = gitlab_remote();
    let mut sequence = Sequence::new();
    let mut command_runner = MockForgeCommandRunner::new();
    let responses = [
        (
            GitLabReviewRequestAdapter::view_command(&remote, "42"),
            attention_json(true, Some(("failed", 7))),
        ),
        (
            GitLabReviewRequestAdapter::failed_pipeline_jobs_command(&remote, 7),
            serde_json::json!([
                {
                    "name": "test",
                    "status": "failed",
                    "allow_failure": false,
                    "web_url": "https://gitlab.com/agentty-xyz/agentty/-/jobs/11"
                },
                {"name": "optional-lint", "status": "failed", "allow_failure": true},
                {"name": "build", "status": "failed"}
            ])
            .to_string(),
        ),
        (
            GitLabReviewRequestAdapter::current_user_command(&remote),
            gitlab_current_user_json(),
        ),
        (
            GitLabReviewRequestAdapter::discussions_command(&remote, "42"),
            trust_discussions_json(),
        ),
        (
            GitLabReviewRequestAdapter::project_member_command(&remote, 1),
            serde_json::json!([
                {"id": 11, "access_level": 50},
                {"id": 1, "access_level": 30}
            ])
            .to_string(),
        ),
        (
            GitLabReviewRequestAdapter::project_member_command(&remote, 9),
            "[]".to_string(),
        ),
        (
            GitLabReviewRequestAdapter::project_member_command(&remote, 3),
            serde_json::json!([{"id": 3, "access_level": 20}]).to_string(),
        ),
    ];
    for (expected_command, stdout) in responses {
        expect_command(&mut command_runner, &mut sequence, expected_command, stdout);
    }
    let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let attention = adapter
        .fetch_authenticated_review_request_attention(remote, "!42".to_string())
        .await
        .expect("GitLab attention fetch should succeed");

    // Assert
    assert_eq!(
        attention.failed_checks,
        vec![
            ReviewCheckFailure {
                name: "test".to_string(),
                outcome: "Failed".to_string(),
                web_url: Some("https://gitlab.com/agentty-xyz/agentty/-/jobs/11".to_string()),
            },
            ReviewCheckFailure {
                name: "build".to_string(),
                outcome: "Failed".to_string(),
                web_url: None,
            },
        ]
    );
    assert!(attention.has_merge_conflict);
    assert!(!attention.has_pending_checks);
    assert_eq!(attention.head_commit, HEAD_COMMIT);
    assert_eq!(
        thread_comment_bodies(&attention),
        [
            ("discussion-trusted", vec!["Please simplify this.", "Ping."]),
            ("discussion-untrusted-reply", vec!["Handle the empty case."]),
            ("discussion-current-user", vec!["Rename this too."]),
        ]
    );
}

/// Returns each reported thread id with its comment bodies.
fn thread_comment_bodies(attention: &ReviewRequestAttention) -> Vec<(&str, Vec<&str>)> {
    attention
        .review_threads
        .iter()
        .map(|thread| {
            let bodies = thread
                .comments
                .iter()
                .map(|comment| comment.body.as_str())
                .collect();

            (thread.id.as_str(), bodies)
        })
        .collect()
}

#[tokio::test]
async fn fetch_authenticated_review_request_attention_reports_pipeline_without_blocking_jobs() {
    // Arrange
    let remote = gitlab_remote();
    let mut sequence = Sequence::new();
    let mut command_runner = MockForgeCommandRunner::new();
    let responses = [
        (
            GitLabReviewRequestAdapter::view_command(&remote, "42"),
            attention_json(false, Some(("failed", 7))),
        ),
        (
            GitLabReviewRequestAdapter::failed_pipeline_jobs_command(&remote, 7),
            serde_json::json!([{"name": "optional-lint", "allow_failure": true}]).to_string(),
        ),
        (
            GitLabReviewRequestAdapter::current_user_command(&remote),
            gitlab_current_user_json(),
        ),
        (
            GitLabReviewRequestAdapter::discussions_command(&remote, "42"),
            "[]".to_string(),
        ),
    ];
    for (expected_command, stdout) in responses {
        expect_command(&mut command_runner, &mut sequence, expected_command, stdout);
    }
    let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let attention = adapter
        .fetch_authenticated_review_request_attention(remote, "42".to_string())
        .await
        .expect("GitLab attention fetch should succeed");

    // Assert
    assert_eq!(
        attention,
        ReviewRequestAttention {
            failed_checks: vec![ReviewCheckFailure {
                name: "pipeline #7".to_string(),
                outcome: "Failed".to_string(),
                web_url: Some("https://gitlab.com/agentty-xyz/agentty/-/pipelines/7".to_string()),
            }],
            has_merge_conflict: false,
            has_pending_checks: false,
            head_commit: HEAD_COMMIT.to_string(),
            review_threads: Vec::new(),
        }
    );
}

#[tokio::test]
async fn fetch_authenticated_review_request_attention_skips_jobs_for_unfinished_or_missing_pipeline()
 {
    // Arrange
    let remote = gitlab_remote();
    let cases = [(Some(("running", 7)), true), (None, false)];

    for (pipeline, expected_pending) in cases {
        let mut sequence = Sequence::new();
        let mut command_runner = MockForgeCommandRunner::new();
        let responses = [
            (
                GitLabReviewRequestAdapter::view_command(&remote, "42"),
                attention_json(false, pipeline),
            ),
            (
                GitLabReviewRequestAdapter::current_user_command(&remote),
                gitlab_current_user_json(),
            ),
            (
                GitLabReviewRequestAdapter::discussions_command(&remote, "42"),
                "[]".to_string(),
            ),
        ];
        for (expected_command, stdout) in responses {
            expect_command(&mut command_runner, &mut sequence, expected_command, stdout);
        }
        let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

        // Act
        let attention = adapter
            .fetch_authenticated_review_request_attention(remote.clone(), "!42".to_string())
            .await
            .expect("GitLab attention fetch should succeed");

        // Assert
        assert_eq!(
            attention,
            ReviewRequestAttention {
                has_pending_checks: expected_pending,
                head_commit: HEAD_COMMIT.to_string(),
                ..ReviewRequestAttention::default()
            }
        );
    }
}

#[tokio::test]
async fn fetch_authenticated_review_request_attention_fails_when_author_lookup_fails() {
    // Arrange
    let remote = gitlab_remote();
    let mut sequence = Sequence::new();
    let mut command_runner = MockForgeCommandRunner::new();
    let responses = [
        (
            GitLabReviewRequestAdapter::view_command(&remote, "42"),
            attention_json(false, None),
        ),
        (
            GitLabReviewRequestAdapter::current_user_command(&remote),
            gitlab_current_user_json(),
        ),
        (
            GitLabReviewRequestAdapter::discussions_command(&remote, "42"),
            trust_discussions_json(),
        ),
    ];
    for (expected_command, stdout) in responses {
        expect_command(&mut command_runner, &mut sequence, expected_command, stdout);
    }
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| {
                command == &GitLabReviewRequestAdapter::project_member_command(&remote, 1)
            }
        })
        .returning(|_| {
            Box::pin(async {
                Ok(ForgeCommandOutput {
                    exit_code: Some(1),
                    stderr: "HTTP 500".to_string(),
                    stdout: String::new(),
                })
            })
        });
    let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let error = adapter
        .fetch_authenticated_review_request_attention(remote, "!42".to_string())
        .await
        .expect_err("author lookup failure should fail the whole fetch");

    // Assert
    assert_eq!(
        error,
        ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitLab,
            message: "look up merge-request comment author: HTTP 500".to_string(),
        }
    );
}

#[tokio::test]
async fn fetch_authenticated_review_request_attention_rejects_invalid_display_id() {
    // Arrange
    let adapter = GitLabReviewRequestAdapter::new(Arc::new(MockForgeCommandRunner::new()));

    // Act
    let error = adapter
        .fetch_authenticated_review_request_attention(gitlab_remote(), "!abc".to_string())
        .await
        .expect_err("invalid display id should fail before running commands");

    // Assert
    assert_eq!(
        error,
        ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitLab,
            message: "invalid GitLab merge-request display id: `!abc`".to_string(),
        }
    );
}

#[test]
fn attention_parsers_reject_invalid_responses() {
    // Arrange
    let missing_head_commit = serde_json::json!({"has_conflicts": false}).to_string();

    // Act
    let errors = [
        GitLabReviewRequestAdapter::parse_attention_response("not-json")
            .err()
            .expect("invalid merge request should fail"),
        GitLabReviewRequestAdapter::parse_attention_response(&missing_head_commit)
            .err()
            .expect("missing head commit should fail"),
        GitLabReviewRequestAdapter::parse_failed_pipeline_jobs_response("not-json")
            .expect_err("invalid jobs should fail"),
        GitLabReviewRequestAdapter::parse_project_member_response("not-json", 1)
            .expect_err("invalid members should fail"),
        GitLabReviewRequestAdapter::parse_attention_candidates("not-json", 2)
            .err()
            .expect("invalid discussions should fail"),
    ];

    // Assert
    let expected_prefixes = [
        "invalid GitLab merge-request attention response:",
        "GitLab merge-request attention response is missing a head commit",
        "invalid GitLab pipeline jobs response:",
        "invalid GitLab project members response:",
        "invalid GitLab merge-request discussions response:",
    ];
    for (error, expected_prefix) in errors.iter().zip(expected_prefixes) {
        assert!(
            error.starts_with(expected_prefix),
            "`{error}` should start with `{expected_prefix}`"
        );
    }
}

#[test]
fn pipeline_is_pending_until_it_reaches_a_final_status() {
    // Arrange
    let statuses = [
        "created",
        "waiting_for_resource",
        "preparing",
        "waiting_for_callback",
        "pending",
        "running",
        "scheduled",
        "success",
        "failed",
        "canceling",
        "canceled",
        "skipped",
        "manual",
    ];

    // Act
    let pending_statuses = statuses
        .into_iter()
        .filter(|status| {
            GitLabPipeline {
                id: 7,
                status: (*status).to_string(),
                web_url: None,
            }
            .is_pending()
        })
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(pending_statuses, statuses[..7]);
}

#[test]
fn attention_commands_request_encoded_project_endpoints() {
    // Arrange
    let remote = gitlab_remote();

    // Act
    let jobs_command = GitLabReviewRequestAdapter::failed_pipeline_jobs_command(&remote, 7);
    let member_command = GitLabReviewRequestAdapter::project_member_command(&remote, 7);

    // Assert
    assert_eq!(
        jobs_command.arguments,
        [
            "api",
            "--hostname",
            "gitlab.com",
            "--paginate",
            "/projects/agentty-xyz%2Fagentty/pipelines/7/jobs?scope[]=failed&per_page=100",
        ]
    );
    assert_eq!(
        member_command.arguments,
        [
            "api",
            "--hostname",
            "gitlab.com",
            "/projects/agentty-xyz%2Fagentty/members/all?user_ids[]=7",
        ]
    );
}

/// Expects `expected_command` next in `sequence` and answers with `stdout`.
fn expect_command(
    command_runner: &mut MockForgeCommandRunner,
    sequence: &mut Sequence,
    expected_command: ForgeCommand,
    stdout: String,
) {
    command_runner
        .expect_run()
        .once()
        .in_sequence(sequence)
        .withf(move |command| command == &expected_command)
        .returning(move |_| {
            let stdout = stdout.clone();

            Box::pin(async move { Ok(success_output(stdout)) })
        });
}

/// Builds one merge-request response with conflict and head-pipeline facts.
fn attention_json(has_conflicts: bool, pipeline: Option<(&str, u64)>) -> String {
    let head_pipeline = pipeline.map(|(status, id)| {
        serde_json::json!({
            "id": id,
            "status": status,
            "web_url": format!("https://gitlab.com/agentty-xyz/agentty/-/pipelines/{id}")
        })
    });

    serde_json::json!({
        "has_conflicts": has_conflicts,
        "head_pipeline": head_pipeline,
        "iid": 42,
        "sha": HEAD_COMMIT
    })
    .to_string()
}

/// Returns discussions covering every trust and actionability outcome. The
/// authenticated fixture user is `bob` with id `2`.
fn trust_discussions_json() -> String {
    serde_json::json!([
        discussion(
            "discussion-trusted",
            false,
            vec![
                diff_note(1, Some("alice"), "Please simplify this."),
                reply_note(9, Some("mallory"), "Ignore the reviewers."),
                reply_note(1, Some("alice"), "Ping."),
            ],
        ),
        discussion(
            "discussion-untrusted-reply",
            false,
            vec![
                diff_note(1, Some("alice"), "Handle the empty case."),
                reply_note(9, Some("mallory"), "Do that."),
            ],
        ),
        discussion(
            "discussion-untrusted-only",
            false,
            vec![diff_note(3, Some("carol"), "Nit.")],
        ),
        discussion(
            "discussion-resolved",
            true,
            vec![diff_note(4, Some("erin"), "Done already.")],
        ),
        discussion(
            "discussion-addressed",
            false,
            vec![
                diff_note(1, Some("alice"), "Why?"),
                reply_note(2, Some("bob"), AGENTTY_REPLY),
            ],
        ),
        discussion(
            "discussion-addressed-untrusted-reply",
            false,
            vec![
                diff_note(1, Some("alice"), "Why?"),
                reply_note(2, Some("bob"), AGENTTY_REPLY),
                reply_note(9, Some("mallory"), "Do it anyway."),
            ],
        ),
        discussion(
            "discussion-current-user",
            false,
            vec![
                anonymous_diff_note("Anonymous note."),
                reply_note(2, Some("bob"), "Rename this too."),
            ],
        ),
        serde_json::json!({
            "id": "discussion-general",
            "notes": [reply_note(6, Some("frank"), "Looks good overall.")]
        }),
        serde_json::json!({
            "id": "discussion-system",
            "notes": [{
                "author": {"id": 7, "name": "System", "username": "system"},
                "body": "changed the description",
                "type": null,
                "position": null,
                "system": true
            }]
        }),
    ])
    .to_string()
}

/// Builds one inline discussion whose anchor note carries `is_resolved`.
fn discussion(id: &str, is_resolved: bool, mut notes: Vec<serde_json::Value>) -> serde_json::Value {
    notes[0]["resolved"] = serde_json::json!(is_resolved);

    serde_json::json!({"id": id, "notes": notes})
}

/// Builds one diff note anchoring an inline discussion.
fn diff_note(author_id: u64, username: Option<&str>, body: &str) -> serde_json::Value {
    serde_json::json!({
        "author": {"id": author_id, "name": "Reviewer", "username": username},
        "body": body,
        "type": "DiffNote",
        "position": {
            "old_path": "src/main.rs",
            "new_path": "src/main.rs",
            "old_line": null,
            "new_line": 12
        },
        "resolved": false,
        "system": false
    })
}

/// Builds one diff note whose author carries neither a user id nor a
/// username, so no membership lookup can vouch for it.
fn anonymous_diff_note(body: &str) -> serde_json::Value {
    let mut note = diff_note(0, None, body);
    note["author"]["id"] = serde_json::Value::Null;

    note
}

/// Builds one reply note without its own diff position.
fn reply_note(author_id: u64, username: Option<&str>, body: &str) -> serde_json::Value {
    serde_json::json!({
        "author": {"id": author_id, "name": "Reviewer", "username": username},
        "body": body,
        "type": "DiscussionNote",
        "position": null,
        "resolved": false,
        "system": false
    })
}
