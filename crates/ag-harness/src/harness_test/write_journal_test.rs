use std::num::NonZeroUsize;

use mockall::Sequence;
use serde_json::json;
use tempfile::tempdir;

use super::support::{model, object_schema, response_without_metadata, write_call};
use crate::harness::Harness;
use crate::model::{ModelError, ModelMessage, ModelResponse};
use crate::repository::Repository;
use crate::session::SessionError;
use crate::tool::Tool;
use crate::turn::TurnError;
use crate::write_journal::WriteStatus;

#[tokio::test]
async fn session_exposes_writes_after_failed_or_evicted_turns_and_reopen() {
    for complete in [false, true] {
        // Arrange
        let mut model = model();
        let mut sequence = Sequence::new();
        model
            .expect_complete()
            .once()
            .in_sequence(&mut sequence)
            .returning(|_| {
                Ok(response_without_metadata(ModelResponse::ToolCall(
                    write_call(
                        "write-call",
                        "--- /dev/null\n+++ b/src/lib.rs\n@@ -0,0 +1 @@\n+new\n",
                    ),
                )))
            });
        model
            .expect_complete()
            .once()
            .in_sequence(&mut sequence)
            .returning(move |_| {
                if complete {
                    Ok(response_without_metadata(ModelResponse::Output(
                        json!({"summary": "done"}),
                    )))
                } else {
                    Err(ModelError::InvalidResponse)
                }
            });
        let directory = tempdir().expect("repository");
        let harness = Harness::new(model)
            .database(directory.path().join("harness.db"))
            .repository(Repository::fixture(directory.path()))
            .max_history_bytes(NonZeroUsize::new(1).expect("positive budget"))
            .allow(Tool::Write);
        let mut session = harness
            .session("session-a", object_schema())
            .create()
            .await
            .expect("session");

        // Act
        let result = session.send("write").await;
        let before = session.writes().await.expect("write outcome");
        drop(session);
        let reopened = harness.resume("session-a").await.expect("reopen");
        let after = reopened.writes().await.expect("durable outcome");
        let history = reopened
            .database
            .load_session("session-a")
            .await
            .expect("history");

        // Assert
        if complete {
            assert_eq!(
                result.expect("completed turn").output(),
                &json!({"summary": "done"})
            );
        } else {
            assert!(matches!(
                result,
                Err(SessionError::Turn(TurnError::Model(
                    ModelError::InvalidResponse
                )))
            ));
        }
        assert_eq!(before, after);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].status, WriteStatus::Applied);
        assert_eq!(after[0].call_id, "write-call");
        assert_eq!(
            tokio::fs::read(directory.path().join("src/lib.rs"))
                .await
                .expect("file"),
            b"new\n"
        );
        assert_eq!(history.turns, Vec::<Vec<ModelMessage>>::new());
        assert_eq!(reopened.history.messages(), Vec::<ModelMessage>::new());
    }
}

#[tokio::test]
async fn session_inspection_reports_storage_failure() {
    // Arrange
    let directory = tempdir().expect("database directory");
    let harness = Harness::new(model()).database(directory.path().join("harness.db"));
    let session = harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session");
    sqlx::query("DROP TABLE session_write")
        .execute(session.database.pool())
        .await
        .expect("remove journal");

    // Act
    let error = session.writes().await.expect_err("storage failure");

    // Assert
    assert!(matches!(
        error,
        SessionError::QueryContext {
            operation: "load persistent writes",
            ..
        }
    ));
}

#[tokio::test]
async fn run_once_writes_without_opening_the_session_database() {
    // Arrange
    let mut model = model();
    let mut sequence = Sequence::new();
    model
        .expect_complete()
        .once()
        .in_sequence(&mut sequence)
        .returning(|_| {
            Ok(response_without_metadata(ModelResponse::ToolCall(
                write_call(
                    "write-call",
                    "--- /dev/null\n+++ b/src/lib.rs\n@@ -0,0 +1 @@\n+new\n",
                ),
            )))
        });
    model
        .expect_complete()
        .once()
        .in_sequence(&mut sequence)
        .returning(|_| {
            Ok(response_without_metadata(ModelResponse::Output(
                json!({"summary": "done"}),
            )))
        });
    let directory = tempdir().expect("repository");
    let database = directory.path().join("unused.db");
    let harness = Harness::new(model)
        .database(&database)
        .repository(Repository::fixture(directory.path()))
        .allow(Tool::Write);

    // Act
    let result = harness
        .run_once("write", object_schema())
        .await
        .expect("stateless write");

    // Assert
    assert_eq!(result.output(), &json!({"summary": "done"}));
    assert_eq!(
        tokio::fs::read(directory.path().join("src/lib.rs"))
            .await
            .expect("file"),
        b"new\n"
    );
    assert!(!database.exists());
}
