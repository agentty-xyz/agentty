use ag_session::SessionMessageKind;

use super::support::prepare_saved_operation;
use crate::AppRepositories;

#[tokio::test]
async fn draft_staging_ends_only_when_saved_prompt_acceptance_commits() {
    // Arrange
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("database");
    prepare_saved_operation(&db).await;
    sqlx::query("UPDATE session SET is_draft = 1 WHERE id = 'first'")
        .execute(&pool)
        .await
        .expect("draft");
    sqlx::query(
        "CREATE TRIGGER reject_transfer BEFORE UPDATE OF prompt ON session_preparation WHEN \
         NEW.prompt IS NULL BEGIN SELECT RAISE(ABORT, 'transfer rejected'); END",
    )
    .execute(&pool)
    .await
    .expect("trigger");

    // Act
    let rejected = db
        .sessions()
        .begin_preparation_prompt_operation("first", "saved payload")
        .await;
    let draft_after_failure = db
        .sessions()
        .load_session("first")
        .await
        .expect("load")
        .expect("session");
    sqlx::query("DROP TRIGGER reject_transfer")
        .execute(&pool)
        .await
        .expect("restore transfer");
    let accepted = db
        .sessions()
        .begin_preparation_prompt_operation("first", "saved payload")
        .await
        .expect("accept");
    let draft_after_acceptance = db
        .sessions()
        .load_session("first")
        .await
        .expect("load")
        .expect("session");

    // Assert
    assert!(rejected.is_err());
    assert!(draft_after_failure.is_draft);
    assert!(accepted);
    assert!(!draft_after_acceptance.is_draft);
}

#[tokio::test]
async fn execution_marker_and_prompt_transfer_are_atomic_and_only_unstarted_failures_retry() {
    // Arrange
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("database");
    prepare_saved_operation(&db).await;
    let sessions = db.sessions();
    sqlx::query(
        "CREATE TRIGGER reject_transfer BEFORE UPDATE OF prompt ON session_preparation WHEN \
         NEW.prompt IS NULL BEGIN SELECT RAISE(ABORT, 'transfer rejected'); END",
    )
    .execute(&pool)
    .await
    .expect("trigger");

    // Act
    let rejected = sessions
        .begin_preparation_prompt_operation("first", "saved payload")
        .await;

    // Assert
    assert!(rejected.is_err());
    let operations = db
        .operations()
        .load_unfinished_session_operations()
        .await
        .expect("operations");
    assert_eq!(operations[0].status, "queued");
    assert!(operations[0].started_at.is_none());
    assert_eq!(
        sessions
            .load_session_preparation("first")
            .await
            .expect("load")
            .expect("row")
            .prompt
            .as_deref(),
        Some("saved payload")
    );

    // Act: only an unstarted failed attempt may release its stable id.
    db.operations()
        .mark_session_operation_failed("workspace:first", "interrupted")
        .await
        .expect("failure");
    sessions
        .reclaim_preparation_prompt_operation("first")
        .await
        .expect("reclaim");
    assert!(
        sessions
            .preparation_prompt_operation_status("first")
            .await
            .expect("status")
            .is_none()
    );
    sqlx::query("DROP TRIGGER reject_transfer")
        .execute(&pool)
        .await
        .expect("restore transfer");
    db.operations()
        .insert_session_operation("workspace:first", "first", "start_prompt")
        .await
        .expect("retry");
    assert!(
        sessions
            .begin_preparation_prompt_operation("first", "saved payload")
            .await
            .expect("begin")
    );
    db.operations()
        .mark_session_operation_failed("workspace:first", "provider failed")
        .await
        .expect("failure after start");
    sessions
        .reclaim_preparation_prompt_operation("first")
        .await
        .expect("do not reclaim execution");

    // Assert
    assert_eq!(
        sessions
            .preparation_prompt_operation_status("first")
            .await
            .expect("status")
            .as_deref(),
        Some("failed")
    );
    assert!(
        !sessions
            .begin_preparation_prompt_operation("first", "saved payload")
            .await
            .expect("already begun")
    );
    assert!(
        sessions
            .load_session_preparation("first")
            .await
            .expect("load")
            .expect("row")
            .prompt
            .is_none()
    );
}

#[tokio::test]
async fn restart_after_marker_commit_retains_the_prompt_without_worker_publication() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("sessions.db");
    let db = crate::Database::open(&path).await.expect("database");
    prepare_saved_operation(&db).await;

    // Act: stop exactly after the start transaction, before live
    // publication.
    assert!(
        db.sessions()
            .begin_preparation_prompt_operation("first", "saved payload [Image #1]")
            .await
            .expect("begin")
    );
    db.pool().close().await;
    drop(db);
    let reopened = crate::Database::open(&path).await.expect("reopen");
    reopened
        .sessions()
        .recover_session_preparations()
        .await
        .expect("recover preparation");
    reopened
        .operations()
        .fail_unfinished_session_operations("restart")
        .await
        .expect("recover operations");
    reopened
        .sessions()
        .reclaim_preparation_prompt_operation("first")
        .await
        .expect("keep started operation");
    let messages = reopened
        .sessions()
        .load_session_messages("first")
        .await
        .expect("transcript");

    // Assert
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content, "saved payload [Image #1]");
    assert_eq!(messages[0].kind, "user_prompt");
    assert_eq!(messages[0].position, 0);
    assert!(
        reopened
            .sessions()
            .load_session_preparation("first")
            .await
            .expect("load")
            .expect("row")
            .prompt
            .is_none()
    );
    assert_eq!(
        reopened
            .sessions()
            .preparation_prompt_operation_status("first")
            .await
            .expect("operation")
            .as_deref(),
        Some("failed")
    );
}

#[tokio::test]
async fn initial_publication_reuses_history_but_fork_replies_append() {
    for (kind, expected_count) in [("start_prompt", 1), ("reply", 2)] {
        // Arrange
        let (db, pool) = AppRepositories::in_memory_with_pool()
            .await
            .expect("database");
        prepare_saved_operation(&db).await;
        sqlx::query("UPDATE session_operation SET kind = ? WHERE id = 'workspace:first'")
            .bind(kind)
            .execute(&pool)
            .await
            .expect("kind");
        db.sessions()
            .append_session_message("first", SessionMessageKind::UserPrompt, "  repeated prompt")
            .await
            .expect("legacy transcript");

        // Act
        assert!(
            db.sessions()
                .begin_preparation_prompt_operation("first", "\n  repeated prompt \n")
                .await
                .expect("begin")
        );
        let messages = db
            .sessions()
            .load_session_messages("first")
            .await
            .expect("messages");

        // Assert
        assert_eq!(messages.len(), expected_count);
        for (position, message) in messages.iter().enumerate() {
            assert_eq!(message.position, i64::try_from(position).expect("position"));
            assert_eq!(message.content, "  repeated prompt");
        }
    }
}
