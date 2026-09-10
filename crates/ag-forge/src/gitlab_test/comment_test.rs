use std::sync::Arc;

use mockall::Sequence;

use super::support_test::{
    gitlab_current_user_json, gitlab_discussions_json, gitlab_remote, success_output,
};
use crate::client::ReviewRequestAdapter;
use crate::command::MockForgeCommandRunner;
use crate::gitlab::GitLabReviewRequestAdapter;
use crate::model::{ForgeKind, ReviewCommentAnchorSide, ReviewRequestError};

#[tokio::test]
async fn fetch_authenticated_review_comment_snapshot_parses_discussions_response() {
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

            move |command| command == &GitLabReviewRequestAdapter::current_user_command(&remote)
        })
        .returning(|_| Box::pin(async { Ok(success_output(gitlab_current_user_json())) }));
    command_runner
        .expect_run()
        .once()
        .in_sequence(&mut sequence)
        .withf({
            let remote = remote.clone();

            move |command| {
                command == &GitLabReviewRequestAdapter::discussions_command(&remote, "42")
            }
        })
        .returning(|_| Box::pin(async { Ok(success_output(gitlab_discussions_json())) }));
    let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let snapshot = adapter
        .fetch_authenticated_review_comment_snapshot(remote, "!42".to_string())
        .await
        .expect("GitLab discussion snapshot should parse");

    // Assert
    assert_eq!(snapshot.threads.len(), 1);
    let thread = &snapshot.threads[0];
    assert_eq!(thread.id, "discussion-1");
    assert_eq!(thread.path, "src/main.rs");
    assert_eq!(thread.line, Some(12));
    assert_eq!(thread.anchor_side, ReviewCommentAnchorSide::New);
    assert_eq!(thread.is_outdated, None);
    assert!(!thread.is_resolved);
    assert_eq!(thread.comments.len(), 2);
    assert_eq!(thread.comments[0].author, "alice");
    assert!(!thread.comments[0].authored_by_current_user);
    assert_eq!(thread.comments[0].body, "Please simplify this.");
    assert!(thread.comments[1].authored_by_current_user);
    assert!(thread.is_addressed_by_agentty());

    assert_eq!(snapshot.pr_level_comments.len(), 1);
    assert_eq!(snapshot.pr_level_comments[0].author, "carol");
}

#[tokio::test]
async fn review_thread_reply_and_resolution_run_discussion_requests() {
    // Arrange
    let remote = gitlab_remote();
    let thread_id = "discussion/1";
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
                    == &GitLabReviewRequestAdapter::reply_to_thread_command(
                        &remote,
                        "42",
                        "discussion/1",
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
                command
                    == &GitLabReviewRequestAdapter::resolve_thread_command(
                        &remote,
                        "42",
                        "discussion/1",
                    )
            }
        })
        .returning(|_| Box::pin(async { Ok(success_output(String::new())) }));
    let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let reply_result = adapter
        .reply_to_authenticated_thread(
            remote.clone(),
            "!42".to_string(),
            thread_id.to_string(),
            "Addressed.".to_string(),
        )
        .await;
    let resolution_result = adapter
        .resolve_authenticated_thread(remote, "!42".to_string(), thread_id.to_string())
        .await;

    // Assert
    assert_eq!(reply_result, Ok(()));
    assert_eq!(resolution_result, Ok(()));
    let encoded_endpoint = GitLabReviewRequestAdapter::discussion_endpoint(
        &gitlab_remote(),
        "42",
        "discussion/1",
        Some("notes"),
    );
    assert!(encoded_endpoint.contains("discussion%2F1/notes"));
}

#[tokio::test]
async fn fetch_authenticated_review_comment_snapshot_preserves_current_user_parse_failure() {
    // Arrange
    let remote = gitlab_remote();
    let mut command_runner = MockForgeCommandRunner::new();
    command_runner
        .expect_run()
        .once()
        .withf({
            let remote = remote.clone();

            move |command| command == &GitLabReviewRequestAdapter::current_user_command(&remote)
        })
        .returning(|_| Box::pin(async { Ok(success_output("not-json".to_string())) }));
    let adapter = GitLabReviewRequestAdapter::new(Arc::new(command_runner));

    // Act
    let error = adapter
        .fetch_authenticated_review_comment_snapshot(remote, "!42".to_string())
        .await
        .expect_err("invalid current user should fail");

    // Assert
    assert!(matches!(
        error,
        ReviewRequestError::OperationFailed { forge_kind, message }
            if forge_kind == ForgeKind::GitLab
                && message.contains("invalid GitLab current-user response")
    ));
}

#[test]
fn discussions_command_requests_all_gitlab_pages() {
    // Arrange
    let remote = gitlab_remote();

    // Act
    let command = GitLabReviewRequestAdapter::discussions_command(&remote, "42");

    // Assert
    assert!(
        command
            .arguments
            .iter()
            .any(|argument| argument == "--paginate")
    );
    assert!(command.arguments.iter().any(|argument| {
        argument == "/projects/agentty-xyz%2Fagentty/merge_requests/42/discussions?per_page=100"
    }));
}

#[test]
fn discussion_snapshot_preserves_old_and_file_anchors() {
    // Arrange
    let cases = [
        (
            serde_json::json!({"old_line": 12, "old_path": "old.rs"}),
            ReviewCommentAnchorSide::Old,
            "old.rs",
            Some(12),
        ),
        (
            serde_json::json!({"old_line": 12, "new_path": "fallback.rs"}),
            ReviewCommentAnchorSide::Old,
            "fallback.rs",
            Some(12),
        ),
        (
            serde_json::json!({"new_path": "file.rs"}),
            ReviewCommentAnchorSide::File,
            "file.rs",
            None,
        ),
        (
            serde_json::json!({"old_path": "fallback.rs"}),
            ReviewCommentAnchorSide::File,
            "fallback.rs",
            None,
        ),
        (
            serde_json::json!({"new_line": 8, "old_path": "fallback.rs"}),
            ReviewCommentAnchorSide::New,
            "fallback.rs",
            Some(8),
        ),
    ];
    for (position, anchor_side, path, line) in cases {
        let mut discussions: serde_json::Value = serde_json::from_str(&gitlab_discussions_json())
            .expect("discussion fixture should parse");
        discussions[0]["notes"][0]["position"] = position;

        // Act
        let snapshot = GitLabReviewRequestAdapter::parse_review_comment_snapshot_response(
            &discussions.to_string(),
            2,
        )
        .expect("discussion anchor should parse");

        // Assert
        assert_eq!(snapshot.threads.len(), 1);
        let thread = &snapshot.threads[0];
        assert_eq!(thread.anchor_side, anchor_side);
        assert_eq!(thread.path, path);
        assert_eq!(thread.line, line);
        assert_eq!(thread.comments[0].body, "Please simplify this.");
    }
}

#[test]
fn discussion_snapshot_ignores_empty_and_system_only_discussions() {
    // Arrange
    let mut discussions: Vec<serde_json::Value> =
        serde_json::from_str(&gitlab_discussions_json()).expect("discussion fixture should parse");
    let mut system_discussion = discussions[0].clone();
    for note in system_discussion["notes"]
        .as_array_mut()
        .expect("notes should be an array")
    {
        note["system"] = serde_json::json!(true);
    }
    discussions.push(system_discussion);
    discussions.push(serde_json::json!({"id": "empty", "notes": []}));

    // Act
    let snapshot = GitLabReviewRequestAdapter::parse_review_comment_snapshot_response(
        &serde_json::to_string(&discussions).expect("discussions should serialize"),
        2,
    )
    .expect("discussion snapshot should parse");

    // Assert
    assert_eq!(snapshot.threads.len(), 1);
    assert_eq!(snapshot.threads[0].id, "discussion-1");
    assert_eq!(snapshot.pr_level_comments.len(), 1);
    assert_eq!(snapshot.pr_level_comments[0].body, "Looks good overall.");
}
