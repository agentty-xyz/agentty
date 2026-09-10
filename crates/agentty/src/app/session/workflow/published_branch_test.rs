use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_forge as forge;
use ag_forge::{MockReviewRequestClient, ReviewCommentAnchorSide, ReviewCommentThread};
use ag_git::{GitError, MockGitClient};
use ag_protocol::{ReviewCommentOutcome, ReviewCommentResolution};
use tokio::sync::mpsc;

use super::{
    PublishedBranchAutoPushInput, ReviewRequestMetadataEvaluationInput,
    ReviewRequestMetadataSyncInput, resolve_review_comments_after_push, review_comment_reply_body,
    sync_linked_review_request_metadata_after_push,
};
use crate::domain::agent::AgentSelection;
use crate::domain::session::{ReviewRequest, ReviewRequestState};
use crate::domain::session_message::SessionTranscript;
use crate::infra::db::AppRepositories;

#[derive(Clone, Copy)]
enum TestCommitComparison {
    Error,
    Matching,
    Rewritten,
    Unbound,
}

#[tokio::test]
async fn metadata_sync_reconciles_live_remote_metadata_without_persisted_baselines() {
    // Arrange
    let db = linked_review_request_db().await;
    let mut git_client = MockGitClient::new();
    git_client.expect_repo_url().once().returning(|_| {
        Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
    });
    let mut review_request_client = MockReviewRequestClient::new();
    review_request_client
        .expect_detect_remote()
        .once()
        .returning(|_| Ok(github_remote()));
    review_request_client
        .expect_review_request_metadata()
        .once()
        .withf(|_, display_id| display_id == "#42")
        .returning(|_, _| {
            Box::pin(async {
                Ok(forge::ReviewRequestMetadata {
                    body: "Tracks #42: https://example.com/issues/42".to_string(),
                    title: "Manual stable title".to_string(),
                })
            })
        });
    review_request_client
        .expect_sync_review_request_metadata()
        .once()
        .withf(|_, display_id, input| {
            display_id == "#42"
                && input.title.as_ref().is_some_and(|title| {
                    title.current == "Manual stable title" && title.desired == "Manual stable title"
                })
                && input.body.as_ref().is_some_and(|body| {
                    body.current == "Tracks #42: https://example.com/issues/42"
                        && body.desired == "Tracks #42: https://example.com/issues/42\n\nNew body."
                })
        })
        .returning(|_, _, _| {
            Box::pin(async {
                Ok(forge::ReviewRequestSummary {
                    display_id: "#42".to_string(),
                    forge_kind: forge::ForgeKind::GitHub,
                    source_branch: "wt/session-id".to_string(),
                    state: ReviewRequestState::Open,
                    status_summary: None,
                    target_branch: "main".to_string(),
                    title: "Manual stable title".to_string(),
                    web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
                })
            })
        });
    let mut one_shot_client = ag_agent::MockOneShotClient::new();
    one_shot_client.expect_submit().once().returning(|request| {
            assert!(request.prompt.contains("https://example.com/issues/42"));

            Ok(ag_agent::OneShotSubmission {
                response: ag_protocol::AgentResponse::plain(
                    r#"{"title":"Manual stable title","description":"Tracks #42: https://example.com/issues/42\n\nNew body.","is_title_change_significant":false}"#,
                ),
                stats: ag_agent::SessionStats::default(),
            })
        });
    let (input, transcript) = metadata_sync_test_input(db.clone(), git_client);
    let metadata_sync_input = ReviewRequestMetadataSyncInput {
        clock: Arc::new(crate::infra::clock::RealClock),
        commit_message: Some("Generated title\n\nNew body.".to_string()),
        evaluation: ReviewRequestMetadataEvaluationInput {
            one_shot_client: Arc::new(one_shot_client),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Codex,
                crate::domain::agent::AgentModel::Gpt56Sol,
            ),
        },
        review_request_client: Arc::new(review_request_client),
    };

    // Act
    sync_linked_review_request_metadata_after_push(&input, &metadata_sync_input).await;
    let review_request = db
        .reviews()
        .load_session_review_request("session-id")
        .await
        .expect("failed to load linked review request")
        .expect("review request should remain linked");

    // Assert
    assert_eq!(review_request.title, "Manual stable title");
    assert!(transcript.lock().expect("transcript lock").is_empty());
}

#[tokio::test]
async fn metadata_sync_reports_link_load_failure() {
    // Arrange
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    insert_session(&db).await;
    link_open_review_request(&db, "#42").await;
    let (input, transcript) = metadata_sync_test_input(db, MockGitClient::new());
    let metadata_sync_input = metadata_sync_input(
        Some("Generated title\n\nNew body."),
        MockReviewRequestClient::new(),
    );
    pool.close().await;

    // Act
    sync_linked_review_request_metadata_after_push(&input, &metadata_sync_input).await;

    // Assert
    assert!(last_transcript_message(&transcript).contains(
        "Failed to update linked review-request metadata: attempted to acquire a connection on a \
         closed pool"
    ));
}

#[tokio::test]
async fn metadata_sync_skips_missing_commit_message() {
    // Arrange
    let db = linked_review_request_db().await;
    let mut git_client = MockGitClient::new();
    git_client
        .expect_head_commit_message()
        .once()
        .returning(|_| Box::pin(async { Ok(None) }));
    let (input, transcript) = metadata_sync_test_input(db, git_client);
    let metadata_sync_input = metadata_sync_input(None, MockReviewRequestClient::new());

    // Act
    sync_linked_review_request_metadata_after_push(&input, &metadata_sync_input).await;

    // Assert
    assert!(transcript.lock().expect("transcript lock").is_empty());
}

#[tokio::test]
async fn metadata_sync_skips_unstructured_commit_message() {
    // Arrange
    let db = linked_review_request_db().await;
    let mut git_client = MockGitClient::new();
    git_client.expect_head_commit_message().never();
    let (input, transcript) = metadata_sync_test_input(db, git_client);
    let metadata_sync_input = metadata_sync_input(Some("\n \n"), MockReviewRequestClient::new());

    // Act
    sync_linked_review_request_metadata_after_push(&input, &metadata_sync_input).await;

    // Assert
    assert!(transcript.lock().expect("transcript lock").is_empty());
}

#[tokio::test]
async fn metadata_sync_reports_repository_remote_failure() {
    // Arrange
    let db = linked_review_request_db().await;
    let mut git_client = MockGitClient::new();
    git_client.expect_repo_url().once().returning(|_| {
        Box::pin(async { Err(GitError::OutputParse("missing remote".to_string())) })
    });
    let (input, transcript) = metadata_sync_test_input(db, git_client);
    let mut review_request_client = MockReviewRequestClient::new();
    review_request_client.expect_detect_remote().never();
    let metadata_sync_input =
        metadata_sync_input(Some("Generated title\n\nNew body."), review_request_client);

    // Act
    sync_linked_review_request_metadata_after_push(&input, &metadata_sync_input).await;

    // Assert
    let message = last_transcript_message(&transcript);
    assert!(
        message.contains("Failed to resolve repository remote for review-request metadata sync")
    );
    assert!(message.contains("missing remote"));
}

#[tokio::test]
async fn review_comment_resolution_reports_reply_and_resolution_failures() {
    // Arrange
    let db = linked_review_request_db().await;
    let mut git_client = MockGitClient::new();
    git_client.expect_repo_url().once().returning(|_| {
        Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
    });
    let mut review_request_client = MockReviewRequestClient::new();
    review_request_client
        .expect_detect_remote()
        .once()
        .returning(|_| Ok(github_remote()));
    review_request_client
        .expect_fetch_review_comment_snapshot()
        .once()
        .returning(|_, _| {
            Box::pin(async { Ok(review_comment_snapshot(&["reply-fails", "resolve-fails"])) })
        });
    review_request_client
        .expect_reply_to_thread()
        .withf(|_, _, thread_id, _| thread_id == "reply-fails")
        .once()
        .returning(|_, _, _, _| {
            Box::pin(async {
                Err(forge::ReviewRequestError::OperationFailed {
                    forge_kind: forge::ForgeKind::GitHub,
                    message: "reply rejected".to_string(),
                })
            })
        });
    review_request_client
        .expect_reply_to_thread()
        .withf(|_, _, thread_id, _| thread_id == "resolve-fails")
        .once()
        .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
    review_request_client
        .expect_resolve_thread()
        .withf(|_, _, thread_id| thread_id == "resolve-fails")
        .once()
        .returning(|_, _, _| {
            Box::pin(async {
                Err(forge::ReviewRequestError::OperationFailed {
                    forge_kind: forge::ForgeKind::GitHub,
                    message: "resolve rejected".to_string(),
                })
            })
        });
    let (input, transcript) = resolution_test_input(
        db,
        git_client,
        review_request_client,
        vec![fixed_outcome("reply-fails"), fixed_outcome("resolve-fails")],
    )
    .await;

    // Act
    resolve_review_comments_after_push(&input).await;

    // Assert
    assert_eq!(
        last_transcript_message(&transcript),
        "[Review Comments Warning] Replied to 1 of 2 review thread(s) and resolved 0 of 2 fixed \
         thread(s). The saved operation will retry after the next successful branch push."
    );
}

#[tokio::test]
async fn review_comment_resolution_replies_to_all_outcomes_and_resolves_only_fixed() {
    // Arrange
    let db = linked_review_request_db().await;
    let mut git_client = MockGitClient::new();
    git_client.expect_repo_url().once().returning(|_| {
        Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
    });
    let mut review_request_client = MockReviewRequestClient::new();
    review_request_client
        .expect_detect_remote()
        .once()
        .returning(|_| Ok(github_remote()));
    review_request_client
        .expect_fetch_review_comment_snapshot()
        .once()
        .returning(|_, _| Box::pin(async { Ok(review_comment_snapshot(&["no-change", "fixed"])) }));
    review_request_client
        .expect_reply_to_thread()
        .withf(|_, _, thread_id, body| {
            (thread_id == "no-change"
                && body
                    == "The current implementation is already safe.\n\n<!-- agentty review \
                        resolution:token-no-change -->")
                || (thread_id == "fixed"
                    && body == "Addressed fixed.\n\n<!-- agentty review resolution:token-fixed -->")
        })
        .times(2)
        .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
    review_request_client
        .expect_resolve_thread()
        .withf(|_, _, thread_id| thread_id == "fixed")
        .once()
        .returning(|_, _, _| Box::pin(async { Ok(()) }));
    let (input, transcript) = resolution_test_input(
        db,
        git_client,
        review_request_client,
        vec![no_change_outcome("no-change"), fixed_outcome("fixed")],
    )
    .await;

    // Act
    resolve_review_comments_after_push(&input).await;

    // Assert
    assert_eq!(
        last_transcript_message(&transcript),
        "[Review Comments] Replied to 2 review thread(s) and resolved 1 fixed thread(s)."
    );
}

#[tokio::test]
async fn review_comment_resolution_reuses_matching_reply_before_retrying_resolution() {
    // Arrange
    let db = linked_review_request_db().await;
    let mut git_client = MockGitClient::new();
    git_client.expect_repo_url().once().returning(|_| {
        Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
    });
    let mut live_snapshot = review_comment_snapshot(&["fixed"]);
    live_snapshot.threads[0]
        .comments
        .push(forge::ReviewComment {
            author: "agentty".to_string(),
            authored_by_current_user: true,
            body: review_comment_reply_body("Addressed fixed.", "token-fixed"),
        });
    let mut review_request_client = MockReviewRequestClient::new();
    review_request_client
        .expect_detect_remote()
        .once()
        .returning(|_| Ok(github_remote()));
    review_request_client
        .expect_fetch_review_comment_snapshot()
        .once()
        .return_once(|_, _| Box::pin(async move { Ok(live_snapshot) }));
    review_request_client.expect_reply_to_thread().never();
    review_request_client
        .expect_resolve_thread()
        .withf(|_, _, thread_id| thread_id == "fixed")
        .once()
        .returning(|_, _, _| Box::pin(async { Ok(()) }));
    let (input, transcript) = resolution_test_input(
        db,
        git_client,
        review_request_client,
        vec![fixed_outcome("fixed")],
    )
    .await;
    input
        .db
        .reviews()
        .mark_session_review_comment_resolution_posting("session-id", "token-fixed")
        .await
        .expect("failed to persist posting state");

    // Act
    resolve_review_comments_after_push(&input).await;

    // Assert
    assert_eq!(
        last_transcript_message(&transcript),
        "[Review Comments] Replied to 1 review thread(s) and resolved 1 fixed thread(s)."
    );
}

#[tokio::test]
async fn review_comment_resolution_does_not_trust_matching_reply_while_pending() {
    // Arrange
    let db = linked_review_request_db().await;
    let mut git_client = MockGitClient::new();
    git_client.expect_repo_url().once().returning(|_| {
        Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
    });
    let mut live_snapshot = review_comment_snapshot(&["fixed"]);
    live_snapshot.threads[0]
        .comments
        .push(forge::ReviewComment {
            author: "collaborator".to_string(),
            authored_by_current_user: false,
            body: review_comment_reply_body("Addressed fixed.", "token-fixed"),
        });
    let mut review_request_client = MockReviewRequestClient::new();
    review_request_client
        .expect_detect_remote()
        .once()
        .returning(|_| Ok(github_remote()));
    review_request_client
        .expect_fetch_review_comment_snapshot()
        .once()
        .return_once(|_, _| Box::pin(async move { Ok(live_snapshot) }));
    review_request_client
        .expect_reply_to_thread()
        .once()
        .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
    review_request_client
        .expect_resolve_thread()
        .once()
        .returning(|_, _, _| Box::pin(async { Ok(()) }));
    let (input, transcript) = resolution_test_input(
        db,
        git_client,
        review_request_client,
        vec![fixed_outcome("fixed")],
    )
    .await;

    // Act
    resolve_review_comments_after_push(&input).await;

    // Assert
    assert_eq!(
        last_transcript_message(&transcript),
        "[Review Comments] Replied to 1 review thread(s) and resolved 1 fixed thread(s)."
    );
}

#[tokio::test]
async fn review_comment_resolution_reports_disappeared_live_thread() {
    // Arrange
    let db = linked_review_request_db().await;
    let mut git_client = MockGitClient::new();
    git_client.expect_repo_url().once().returning(|_| {
        Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
    });
    let mut review_request_client = MockReviewRequestClient::new();
    review_request_client
        .expect_detect_remote()
        .once()
        .returning(|_| Ok(github_remote()));
    review_request_client
        .expect_fetch_review_comment_snapshot()
        .once()
        .returning(|_, _| Box::pin(async { Ok(forge::ReviewCommentSnapshot::default()) }));
    review_request_client.expect_reply_to_thread().never();
    review_request_client.expect_resolve_thread().never();
    let (input, transcript) = resolution_test_input(
        db,
        git_client,
        review_request_client,
        vec![fixed_outcome("missing")],
    )
    .await;

    // Act
    resolve_review_comments_after_push(&input).await;

    // Assert
    assert_eq!(
        last_transcript_message(&transcript),
        "[Review Comments Warning] Replied to 0 of 1 review thread(s) and resolved 0 of 1 fixed \
         thread(s). The saved operation will retry after the next successful branch push."
    );
}

#[tokio::test]
async fn review_comment_resolution_reports_live_snapshot_failure() {
    // Arrange
    let db = linked_review_request_db().await;
    let mut git_client = MockGitClient::new();
    git_client.expect_repo_url().once().returning(|_| {
        Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
    });
    let mut review_request_client = MockReviewRequestClient::new();
    review_request_client
        .expect_detect_remote()
        .once()
        .returning(|_| Ok(github_remote()));
    review_request_client
        .expect_fetch_review_comment_snapshot()
        .once()
        .returning(|_, _| {
            Box::pin(async {
                Err(forge::ReviewRequestError::OperationFailed {
                    forge_kind: forge::ForgeKind::GitHub,
                    message: "snapshot unavailable".to_string(),
                })
            })
        });
    review_request_client.expect_reply_to_thread().never();
    review_request_client.expect_resolve_thread().never();
    let (input, transcript) = resolution_test_input(
        db,
        git_client,
        review_request_client,
        vec![fixed_outcome("fixed")],
    )
    .await;

    // Act
    resolve_review_comments_after_push(&input).await;

    // Assert
    assert_eq!(
        last_transcript_message(&transcript),
        "[Review Comments Warning] Replied to 0 of 1 review thread(s) and resolved 0 of 1 fixed \
         thread(s). The saved operation will retry after the next successful branch push."
    );
}

#[tokio::test]
async fn review_comment_resolution_does_not_reply_to_concurrently_resolved_thread() {
    // Arrange
    let db = linked_review_request_db().await;
    let mut git_client = MockGitClient::new();
    git_client.expect_repo_url().once().returning(|_| {
        Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
    });
    let mut live_snapshot = review_comment_snapshot(&["fixed"]);
    live_snapshot.threads[0].is_resolved = true;
    let mut review_request_client = MockReviewRequestClient::new();
    review_request_client
        .expect_detect_remote()
        .once()
        .returning(|_| Ok(github_remote()));
    review_request_client
        .expect_fetch_review_comment_snapshot()
        .once()
        .return_once(|_, _| Box::pin(async move { Ok(live_snapshot) }));
    review_request_client.expect_reply_to_thread().never();
    review_request_client.expect_resolve_thread().never();
    let (input, transcript) = resolution_test_input(
        db,
        git_client,
        review_request_client,
        vec![fixed_outcome("fixed")],
    )
    .await;

    // Act
    resolve_review_comments_after_push(&input).await;

    // Assert
    assert_eq!(
        last_transcript_message(&transcript),
        "[Review Comments Warning] Replied to 0 of 1 review thread(s) and resolved 1 of 1 fixed \
         thread(s). The saved operation will retry after the next successful branch push."
    );
}

#[tokio::test]
async fn review_comment_resolution_reports_repository_remote_failure() {
    // Arrange
    let db = linked_review_request_db().await;
    let mut git_client = MockGitClient::new();
    git_client.expect_repo_url().once().returning(|_| {
        Box::pin(async {
            Err(GitError::CommandFailed {
                command: "git remote get-url origin".to_string(),
                stderr: "missing remote".to_string(),
            })
        })
    });
    let (input, transcript) = resolution_test_input(
        db,
        git_client,
        MockReviewRequestClient::new(),
        vec![fixed_outcome("thread-1")],
    )
    .await;

    // Act
    resolve_review_comments_after_push(&input).await;

    // Assert
    assert_eq!(
        last_transcript_message(&transcript),
        "[Review Comments Warning] Replied to 0 of 1 review thread(s) and resolved 0 of 1 fixed \
         thread(s). The saved operation will retry after the next successful branch push."
    );
}

#[tokio::test]
async fn review_comment_resolution_reports_remote_detection_failure() {
    // Arrange
    let db = linked_review_request_db().await;
    let mut git_client = MockGitClient::new();
    git_client
        .expect_repo_url()
        .once()
        .returning(|_| Box::pin(async { Ok("ssh://example.com/owner/repo.git".to_string()) }));
    let mut review_request_client = MockReviewRequestClient::new();
    review_request_client
        .expect_detect_remote()
        .once()
        .returning(|repo_url| Err(forge::ReviewRequestError::UnsupportedRemote { repo_url }));
    let (input, transcript) = resolution_test_input(
        db,
        git_client,
        review_request_client,
        vec![fixed_outcome("thread-1")],
    )
    .await;

    // Act
    resolve_review_comments_after_push(&input).await;

    // Assert
    assert_eq!(
        last_transcript_message(&transcript),
        "[Review Comments Warning] Replied to 0 of 1 review thread(s) and resolved 0 of 1 fixed \
         thread(s). The saved operation will retry after the next successful branch push."
    );
}

#[tokio::test]
async fn review_comment_resolution_skips_sessions_without_open_linked_review() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    insert_session(&db).await;
    let mut git_client = MockGitClient::new();
    git_client.expect_repo_url().never();
    let (input, transcript) = resolution_test_input(
        db,
        git_client,
        MockReviewRequestClient::new(),
        vec![fixed_outcome("thread-1")],
    )
    .await;

    // Act
    resolve_review_comments_after_push(&input).await;

    // Assert
    assert_eq!(
        last_transcript_message(&transcript),
        "[Review Comments Warning] Skipped 1 review thread update(s) because the session no \
         longer has an open linked review request. The saved operation will retry after the link \
         is restored and the branch is pushed again."
    );
}

#[tokio::test]
async fn review_comment_resolution_reports_operation_load_failure() {
    // Arrange
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    insert_session(&db).await;
    let mut git_client = MockGitClient::new();
    git_client.expect_repo_url().never();
    let (input, transcript) = resolution_test_input(
        db,
        git_client,
        MockReviewRequestClient::new(),
        vec![fixed_outcome("thread-1")],
    )
    .await;
    pool.close().await;

    // Act
    resolve_review_comments_after_push(&input).await;

    // Assert
    assert_eq!(
        last_transcript_message(&transcript),
        "[Review Comments Warning] Could not load saved review-comment operations after the \
         branch push: attempted to acquire a connection on a closed pool"
    );
}

#[tokio::test]
async fn review_comment_resolution_reports_posting_progress_failure() {
    // Arrange
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    insert_session(&db).await;
    link_open_review_request(&db, "#42").await;
    let mut git_client = MockGitClient::new();
    git_client.expect_repo_url().once().returning(|_| {
        Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
    });
    let mut review_request_client = MockReviewRequestClient::new();
    review_request_client
        .expect_detect_remote()
        .once()
        .returning(|_| Ok(github_remote()));
    review_request_client
        .expect_fetch_review_comment_snapshot()
        .once()
        .return_once(move |_, _| {
            Box::pin(async move {
                pool.close().await;

                Ok(review_comment_snapshot(&["fixed"]))
            })
        });
    review_request_client.expect_reply_to_thread().never();
    review_request_client.expect_resolve_thread().never();
    let (input, transcript) = resolution_test_input(
        db,
        git_client,
        review_request_client,
        vec![fixed_outcome("fixed")],
    )
    .await;

    // Act
    resolve_review_comments_after_push(&input).await;

    // Assert
    assert_eq!(
        last_transcript_message(&transcript),
        "[Review Comments Warning] Replied to 0 of 1 review thread(s) and resolved 0 of 1 fixed \
         thread(s). The saved operation will retry after the next successful branch push."
    );
}

#[tokio::test]
async fn review_comment_resolution_counts_replied_thread_resolved_during_retry() {
    // Arrange
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    insert_session(&db).await;
    link_open_review_request(&db, "#42").await;
    let mut git_client = MockGitClient::new();
    git_client.expect_repo_url().once().returning(|_| {
        Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
    });
    let mut live_snapshot = review_comment_snapshot(&["fixed"]);
    live_snapshot.threads[0].is_resolved = true;
    live_snapshot.threads[0]
        .comments
        .push(forge::ReviewComment {
            author: "agentty".to_string(),
            authored_by_current_user: true,
            body: review_comment_reply_body("Addressed fixed.", "token-fixed"),
        });
    let mut review_request_client = MockReviewRequestClient::new();
    review_request_client
        .expect_detect_remote()
        .once()
        .returning(|_| Ok(github_remote()));
    review_request_client
        .expect_fetch_review_comment_snapshot()
        .once()
        .return_once(move |_, _| {
            Box::pin(async move {
                pool.close().await;

                Ok(live_snapshot)
            })
        });
    review_request_client.expect_reply_to_thread().never();
    review_request_client.expect_resolve_thread().never();
    let (input, transcript) = resolution_test_input(
        db,
        git_client,
        review_request_client,
        vec![fixed_outcome("fixed")],
    )
    .await;
    input
        .db
        .reviews()
        .mark_session_review_comment_resolution_posting("session-id", "token-fixed")
        .await
        .expect("failed to persist posting state");

    // Act
    resolve_review_comments_after_push(&input).await;

    // Assert
    assert_eq!(
        last_transcript_message(&transcript),
        "[Review Comments] Replied to 1 review thread(s) and resolved 1 fixed thread(s)."
    );
}

#[tokio::test]
async fn review_comment_resolution_reports_link_load_failure() {
    // Arrange
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    insert_session(&db).await;
    db.reviews()
        .update_session_review_request("session-id", Some(open_review_request("#42")))
        .await
        .expect("failed to link review request");
    persist_resolution_test_operations(&db, vec![fixed_outcome("thread-1")], Some("commit-1"))
        .await;
    let mut git_client = MockGitClient::new();
    git_client
        .expect_get_ref_ahead_behind()
        .once()
        .return_once(move |_, _, _| {
            Box::pin(async move {
                pool.close().await;

                Ok((0, 0))
            })
        });
    git_client.expect_repo_url().never();
    let (input, transcript) =
        persisted_resolution_test_input(db, git_client, MockReviewRequestClient::new());

    // Act
    resolve_review_comments_after_push(&input).await;

    // Assert
    assert_eq!(
        last_transcript_message(&transcript),
        "[Review Comments Warning] Replied to 0 of 1 review thread(s) and resolved 0 of 1 fixed \
         thread(s). The saved operation will retry after the next successful branch push."
    );
}

#[tokio::test]
async fn review_comment_resolution_rejects_operations_for_an_old_review_request() {
    // Arrange
    let db = linked_review_request_db().await;
    let mut git_client = MockGitClient::new();
    git_client.expect_repo_url().never();
    let (input, transcript) = resolution_test_input(
        db.clone(),
        git_client,
        MockReviewRequestClient::new(),
        vec![fixed_outcome("thread-1")],
    )
    .await;
    db.reviews()
        .update_session_review_request("session-id", Some(open_review_request("#43")))
        .await
        .expect("failed to replace linked review request");

    // Act
    resolve_review_comments_after_push(&input).await;
    let operations = db
        .reviews()
        .load_session_review_comment_resolutions("session-id")
        .await
        .expect("failed to load retained review operation");

    // Assert
    assert_eq!(operations.len(), 1);
    assert_eq!(
        last_transcript_message(&transcript),
        "[Review Comments Warning] Skipped 1 review thread update(s) because the session no \
         longer has an open linked review request. The saved operation will retry after the link \
         is restored and the branch is pushed again."
    );
}

#[tokio::test]
async fn review_comment_resolution_discards_fix_commit_removed_by_rebase() {
    // Arrange
    let db = linked_review_request_db().await;
    let mut git_client = MockGitClient::new();
    git_client.expect_repo_url().never();
    let (input, transcript) = resolution_test_input_with_commit_reachability(
        db.clone(),
        git_client,
        MockReviewRequestClient::new(),
        vec![fixed_outcome("thread-1")],
        TestCommitComparison::Rewritten,
    )
    .await;

    // Act
    resolve_review_comments_after_push(&input).await;
    let operations = db
        .reviews()
        .load_session_review_comment_resolutions("session-id")
        .await
        .expect("failed to load discarded review operation");

    // Assert
    assert_eq!(operations, Vec::new());
    assert_eq!(
        last_transcript_message(&transcript),
        "[Review Comments Warning] Discarded 1 saved review thread update(s) because the pushed \
         branch tip no longer exactly matches the reported fix commit. Reopen review comments to \
         retry."
    );
}

#[tokio::test]
async fn review_comment_resolution_retains_unbound_operation_for_fresh_retry() {
    // Arrange
    let db = linked_review_request_db().await;
    let mut git_client = MockGitClient::new();
    git_client.expect_get_ref_ahead_behind().never();
    git_client.expect_repo_url().never();
    let (input, transcript) = resolution_test_input_with_commit_reachability(
        db.clone(),
        git_client,
        MockReviewRequestClient::new(),
        vec![fixed_outcome("thread-1")],
        TestCommitComparison::Unbound,
    )
    .await;

    // Act
    resolve_review_comments_after_push(&input).await;
    let operations = db
        .reviews()
        .load_session_review_comment_resolutions("session-id")
        .await
        .expect("failed to load retained review operation");

    // Assert
    assert_eq!(operations.len(), 1);
    assert!(operations[0].commit_hash.is_none());
    assert_eq!(
        last_transcript_message(&transcript),
        "[Review Comments Warning] Kept 1 saved review thread update(s) pending because Agentty \
         could not finish binding them to the committed revision. Reopen those comments and run a \
         fresh agent turn to retry."
    );
}

#[tokio::test]
async fn review_comment_resolution_retries_when_commit_check_fails() {
    // Arrange
    let db = linked_review_request_db().await;
    let mut git_client = MockGitClient::new();
    git_client.expect_repo_url().never();
    let (input, transcript) = resolution_test_input_with_commit_reachability(
        db.clone(),
        git_client,
        MockReviewRequestClient::new(),
        vec![fixed_outcome("thread-1")],
        TestCommitComparison::Error,
    )
    .await;

    // Act
    resolve_review_comments_after_push(&input).await;
    let operations = db
        .reviews()
        .load_session_review_comment_resolutions("session-id")
        .await
        .expect("failed to load retained review operation");

    // Assert
    assert_eq!(operations.len(), 1);
    assert_eq!(
        last_transcript_message(&transcript),
        "[Review Comments Warning] Could not verify saved review-comment commits after the branch \
         push: commit lookup failed. The saved operations will retry after the next successful \
         push."
    );
}

/// Builds one detached-push input for direct review-resolution tests.
async fn resolution_test_input(
    db: AppRepositories,
    git_client: MockGitClient,
    review_request_client: MockReviewRequestClient,
    outcomes: Vec<ReviewCommentOutcome>,
) -> (PublishedBranchAutoPushInput, Arc<Mutex<SessionTranscript>>) {
    resolution_test_input_with_commit_reachability(
        db,
        git_client,
        review_request_client,
        outcomes,
        TestCommitComparison::Matching,
    )
    .await
}

/// Builds one resolution input with a deterministic commit comparison.
async fn resolution_test_input_with_commit_reachability(
    db: AppRepositories,
    mut git_client: MockGitClient,
    review_request_client: MockReviewRequestClient,
    outcomes: Vec<ReviewCommentOutcome>,
    commit_comparison: TestCommitComparison,
) -> (PublishedBranchAutoPushInput, Arc<Mutex<SessionTranscript>>) {
    if !matches!(commit_comparison, TestCommitComparison::Unbound) {
        git_client
            .expect_get_ref_ahead_behind()
            .returning(move |_, left_ref, right_ref| {
                assert_eq!(left_ref, "HEAD");
                assert_eq!(right_ref, "commit-1");

                Box::pin(async move {
                    match commit_comparison {
                        TestCommitComparison::Error => {
                            Err(GitError::OutputParse("commit lookup failed".to_string()))
                        }
                        TestCommitComparison::Matching => Ok((0, 0)),
                        TestCommitComparison::Rewritten => Ok((1, 1)),
                        TestCommitComparison::Unbound => unreachable!(),
                    }
                })
            });
    }
    let commit_hash =
        (!matches!(commit_comparison, TestCommitComparison::Unbound)).then_some("commit-1");
    persist_resolution_test_operations(&db, outcomes, commit_hash).await;

    persisted_resolution_test_input(db, git_client, review_request_client)
}

/// Persists deterministic review operations for direct resolution tests.
async fn persist_resolution_test_operations(
    db: &AppRepositories,
    outcomes: Vec<ReviewCommentOutcome>,
    commit_hash: Option<&str>,
) {
    let resolutions = outcomes
        .into_iter()
        .map(
            |outcome| crate::infra::db::NewSessionReviewCommentResolution {
                commit_hash: commit_hash.map(str::to_string),
                reply: outcome.reply,
                reply_token: format!("token-{}", outcome.thread_id),
                resolution: match outcome.resolution {
                    ReviewCommentResolution::Fixed => "fixed",
                    ReviewCommentResolution::NoChangeNeeded => "no_change_needed",
                }
                .to_string(),
                review_request_display_id: "#42".to_string(),
                thread_id: outcome.thread_id,
            },
        )
        .collect::<Vec<_>>();
    db.reviews()
        .insert_session_review_comment_resolutions("session-id", &resolutions)
        .await
        .expect("failed to persist review-comment resolutions");
}

/// Builds one resolution input after its durable operations are present.
fn persisted_resolution_test_input(
    db: AppRepositories,
    git_client: MockGitClient,
    review_request_client: MockReviewRequestClient,
) -> (PublishedBranchAutoPushInput, Arc<Mutex<SessionTranscript>>) {
    let transcript = Arc::new(Mutex::new(SessionTranscript::default()));
    let input = PublishedBranchAutoPushInput {
        app_event_tx: mpsc::unbounded_channel().0,
        db,
        folder: PathBuf::from("/tmp/project"),
        git_client: Arc::new(git_client),
        published_upstream_ref: "origin/wt/session-id".to_string(),
        review_request_client: Arc::new(review_request_client),
        review_request_metadata_sync: None,
        session_id: "session-id".into(),
        session_update_versions: Arc::default(),
        sync_operation_id: "sync-id".to_string(),
        transcript: Arc::clone(&transcript),
    };

    (input, transcript)
}

/// Builds one detached-push input for direct metadata-sync tests.
fn metadata_sync_test_input(
    db: AppRepositories,
    git_client: MockGitClient,
) -> (PublishedBranchAutoPushInput, Arc<Mutex<SessionTranscript>>) {
    let transcript = Arc::new(Mutex::new(SessionTranscript::default()));
    let input = PublishedBranchAutoPushInput {
        app_event_tx: mpsc::unbounded_channel().0,
        db,
        folder: PathBuf::from("/tmp/project"),
        git_client: Arc::new(git_client),
        published_upstream_ref: "origin/wt/session-id".to_string(),
        review_request_client: Arc::new(MockReviewRequestClient::new()),
        review_request_metadata_sync: None,
        session_id: "session-id".into(),
        session_update_versions: Arc::default(),
        sync_operation_id: "sync-id".to_string(),
        transcript: Arc::clone(&transcript),
    };

    (input, transcript)
}

/// Builds metadata-sync dependencies with deterministic provider mocks.
fn metadata_sync_input(
    commit_message: Option<&str>,
    review_request_client: MockReviewRequestClient,
) -> ReviewRequestMetadataSyncInput {
    ReviewRequestMetadataSyncInput {
        clock: Arc::new(crate::infra::clock::RealClock),
        commit_message: commit_message.map(str::to_string),
        evaluation: ReviewRequestMetadataEvaluationInput {
            one_shot_client: Arc::new(ag_agent::MockOneShotClient::new()),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Codex,
                crate::domain::agent::AgentModel::Gpt56Sol,
            ),
        },
        review_request_client: Arc::new(review_request_client),
    }
}

/// Builds one fixed outcome accepted by the post-turn allowlist.
fn fixed_outcome(thread_id: &str) -> ReviewCommentOutcome {
    ReviewCommentOutcome {
        reply: format!("Addressed {thread_id}."),
        resolution: ag_protocol::ReviewCommentResolution::Fixed,
        thread_id: thread_id.to_string(),
    }
}

/// Builds one no-change outcome that receives a reply but remains open.
fn no_change_outcome(thread_id: &str) -> ReviewCommentOutcome {
    ReviewCommentOutcome {
        reply: "The current implementation is already safe.".to_string(),
        resolution: ag_protocol::ReviewCommentResolution::NoChangeNeeded,
        thread_id: thread_id.to_string(),
    }
}

/// Builds one live unresolved forge snapshot for outcome-application
/// tests.
fn review_comment_snapshot(thread_ids: &[&str]) -> forge::ReviewCommentSnapshot {
    forge::ReviewCommentSnapshot {
        pr_level_comments: Vec::new(),
        threads: thread_ids
            .iter()
            .map(|thread_id| ReviewCommentThread {
                anchor_side: ReviewCommentAnchorSide::New,
                comments: Vec::new(),
                id: (*thread_id).to_string(),
                is_outdated: Some(false),
                is_resolved: false,
                line: Some(1),
                path: "src/lib.rs".to_string(),
                start_line: None,
            })
            .collect(),
    }
}

/// Inserts one session linked to an open GitHub pull request.
async fn linked_review_request_db() -> AppRepositories {
    let db = AppRepositories::in_memory().await.expect("db should open");
    insert_session(&db).await;
    link_open_review_request(&db, "#42").await;

    db
}

/// Links one open review request to the resolution-test session.
async fn link_open_review_request(db: &AppRepositories, display_id: &str) {
    db.reviews()
        .update_session_review_request("session-id", Some(open_review_request(display_id)))
        .await
        .expect("failed to link review request");
}

/// Builds one open linked review request fixture.
fn open_review_request(display_id: &str) -> ReviewRequest {
    ReviewRequest {
        last_refreshed_at: 100,
        summary: forge::ReviewRequestSummary {
            display_id: display_id.to_string(),
            forge_kind: forge::ForgeKind::GitHub,
            source_branch: "wt/session-id".to_string(),
            state: ReviewRequestState::Open,
            status_summary: None,
            target_branch: "main".to_string(),
            title: "Review title".to_string(),
            web_url: format!(
                "https://github.com/agentty-xyz/agentty/pull/{}",
                display_id.trim_start_matches('#')
            ),
        },
    }
}

/// Inserts the session row required by review and transcript stores.
async fn insert_session(db: &AppRepositories) {
    let project_id = db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    db.sessions()
        .insert_session(
            "session-id",
            "gemini-3.8-flash",
            "main",
            "Review",
            project_id,
        )
        .await
        .expect("failed to insert session");
}

/// Returns one GitHub remote used by the forge mock.
fn github_remote() -> forge::ForgeRemote {
    forge::ForgeRemote {
        command_working_directory: None,
        forge_kind: forge::ForgeKind::GitHub,
        host: "github.com".to_string(),
        namespace: "agentty-xyz".to_string(),
        project: "agentty".to_string(),
        repo_url: "https://github.com/agentty-xyz/agentty.git".to_string(),
        web_url: "https://github.com/agentty-xyz/agentty".to_string(),
    }
}

/// Reads the latest live transcript message.
fn last_transcript_message(transcript: &Arc<Mutex<SessionTranscript>>) -> String {
    transcript
        .lock()
        .expect("transcript lock")
        .messages()
        .last()
        .expect("workflow notice")
        .content
        .trim()
        .to_string()
}
