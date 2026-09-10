use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;
use tempfile::tempdir;

use super::support::{TurnStatusRow, metadata_model, model, schema};
use crate::harness::Harness;
use crate::model::{ModelCompletion, ModelError, ModelMessage, ModelMetadata, ModelResponse};
use crate::session::{Database, NewSession, SessionError, SessionInfo};

#[tokio::test]
async fn session_info_loads_model_identity_and_reports_missing_sessions() {
    // Arrange
    let directory = tempdir().expect("temporary directory should be created");
    let database_path = directory.path().join("harness.db");
    let database = Database::open(&database_path)
        .await
        .expect("database should open");
    let metadata = ModelMetadata::new("provider", "model").expect("metadata should be valid");
    database
        .create_session(
            &NewSession::new("session-a", schema()),
            Some(metadata),
            100_000,
        )
        .await
        .expect("session should be created");

    // Act
    let info = SessionInfo::load(&database_path, "session-a")
        .await
        .expect("session identity should load");
    let missing = SessionInfo::load(&database_path, "missing")
        .await
        .expect_err("missing session should fail");

    // Assert
    assert_eq!(info.provider(), Some("provider"));
    assert_eq!(info.model(), Some("model"));
    assert!(matches!(missing, SessionError::NotFound { .. }));
}

#[tokio::test]
async fn persistent_chat_restores_completed_history_and_system_prompt() {
    // Arrange
    let directory = tempdir().expect("temporary directory should be created");
    let database_path = directory.path().join("harness.db");
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        let expected = if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                ModelMessage::System("persistent instructions".to_string()),
                ModelMessage::User("first".to_string()),
            ]
        } else {
            vec![
                ModelMessage::System("persistent instructions".to_string()),
                ModelMessage::User("first".to_string()),
                ModelMessage::Assistant(r#"{"summary":"one"}"#.to_string()),
                ModelMessage::User("second".to_string()),
            ]
        };
        assert_eq!(request.messages(), expected);

        Ok(ModelCompletion::from_response(ModelResponse::Output(
            json!({
                "summary": if expected.len() == 2 { "one" } else { "two" }
            }),
        )))
    });
    let harness = Harness::new(model).database(&database_path);
    let mut session = harness
        .session("session-a", schema())
        .system_prompt("persistent instructions")
        .create()
        .await
        .expect("session should be created");

    // Act
    let first = session
        .send("first")
        .await
        .expect("first turn should succeed");
    drop(session);
    let mut resumed = harness
        .resume("session-a")
        .await
        .expect("session should reopen");
    let second = resumed
        .send("second")
        .await
        .expect("second turn should succeed");

    // Assert
    assert_eq!(resumed.id(), "session-a");
    assert_eq!(first.output(), &json!({ "summary": "one" }));
    assert_eq!(second.output(), &json!({ "summary": "two" }));
}

#[tokio::test]
async fn persistent_chat_does_not_store_failed_turns() {
    // Arrange
    let directory = tempdir().expect("temporary directory should be created");
    let database_path = directory.path().join("harness.db");
    let mut model = model();
    model
        .expect_complete()
        .times(1)
        .returning(|_| Err(ModelError::InvalidResponse));
    let harness = Harness::new(model).database(&database_path);
    let mut session = harness
        .session("session-a", schema())
        .create()
        .await
        .expect("session should be created");

    // Act
    let error = session.send("failed").await.expect_err("turn should fail");
    let database = Database::open(&database_path)
        .await
        .expect("database should open");
    let row = sqlx::query_as!(
        TurnStatusRow,
        r#"
SELECT COUNT(message.id) AS "message_count!: i64",
       turn.status AS "status!: String"
FROM session_turn AS turn
LEFT JOIN session_message AS message
  ON message.session_id = turn.session_id
 AND message.turn_position = turn.turn_position
WHERE turn.session_id = ?
GROUP BY turn.status
"#,
        "session-a"
    )
    .fetch_one(&database.pool)
    .await
    .expect("message count should load");

    // Assert
    assert!(matches!(error, SessionError::Turn(_)));
    assert_eq!(row.message_count, 1);
    assert_eq!(row.status, "failed");
}

#[tokio::test]
async fn opening_session_validates_saved_model_identity() {
    // Arrange
    let directory = tempdir().expect("temporary directory should be created");
    let database_path = directory.path().join("harness.db");
    let original = Harness::new(metadata_model("provider-a", "model-a")).database(&database_path);
    original
        .session("session-a", schema())
        .create()
        .await
        .expect("session should be created");
    let different = Harness::new(metadata_model("provider-b", "model-b")).database(&database_path);

    // Act
    let mismatch = different
        .resume("session-a")
        .await
        .err()
        .expect("model mismatch should fail");
    let missing = different
        .resume("missing")
        .await
        .err()
        .expect("missing session should fail");

    // Assert
    assert!(matches!(mismatch, SessionError::ModelMismatch { .. }));
    assert!(matches!(missing, SessionError::NotFound { .. }));
}

#[tokio::test]
async fn opening_session_accepts_matching_model_identity() {
    // Arrange
    let directory = tempdir().expect("temporary directory should be created");
    let database_path = directory.path().join("harness.db");
    let original = Harness::new(metadata_model("provider", "model")).database(&database_path);
    original
        .session("session-a", schema())
        .create()
        .await
        .expect("session should be created");
    let matching = Harness::new(metadata_model("provider", "model")).database(&database_path);

    // Act
    let session = matching
        .resume("session-a")
        .await
        .expect("matching session should open");

    // Assert
    assert_eq!(session.id(), "session-a");
}

#[tokio::test]
async fn opening_session_rejects_incomplete_saved_model_identity() {
    // Arrange
    let directory = tempdir().expect("temporary directory should be created");
    let database_path = directory.path().join("harness.db");
    let database = Database::open(&database_path)
        .await
        .expect("database should open");
    database
        .create_session(&NewSession::new("session-a", schema()), None, 100_000)
        .await
        .expect("session should be created");
    let mut connection = database
        .pool
        .acquire()
        .await
        .expect("connection should open");
    sqlx::query("PRAGMA ignore_check_constraints = ON")
        .execute(&mut *connection)
        .await
        .expect("constraints should be disabled for corruption fixture");
    sqlx::query("UPDATE session SET provider = 'provider' WHERE id = 'session-a'")
        .execute(&mut *connection)
        .await
        .expect("model identity should be corrupted");
    drop(connection);
    let harness = Harness::new(model()).database(&database_path);

    // Act
    let error = harness
        .resume("session-a")
        .await
        .err()
        .expect("incomplete identity should fail");

    // Assert
    assert!(matches!(error, SessionError::InvalidData { .. }));
}

#[tokio::test]
async fn persistent_chat_uses_saved_history_budget_when_reopened() {
    // Arrange
    let directory = tempdir().expect("temporary directory should be created");
    let database_path = directory.path().join("harness.db");
    let harness = Harness::new(model())
        .database(&database_path)
        .max_history_bytes(NonZeroUsize::new(64).expect("history limit should be nonzero"));
    let session = harness
        .session("session-a", schema())
        .create()
        .await
        .expect("session should be created");
    drop(session);

    // Act
    let _reopened = harness
        .resume("session-a")
        .await
        .expect("session should reopen");

    // Assert
    let loaded = Database::open(&database_path)
        .await
        .expect("database should open")
        .load_session("session-a")
        .await
        .expect("session should load");
    assert_eq!(loaded.max_history_bytes, 64);
}
