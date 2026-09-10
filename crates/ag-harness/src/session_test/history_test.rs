use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use tempfile::tempdir;

use super::support::{schema, turn};
use crate::model::ModelMessage;
use crate::session::{Database, NewSession, SessionError, TimestampSource, load_turn_size_page};

#[tokio::test]
async fn database_loads_only_newest_complete_turns_within_budget() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");
    let older_turn = turn("older question", "older answer");
    let latest_turn = turn("latest question", "latest answer");
    let latest_bytes = latest_turn.iter().map(ModelMessage::retained_bytes).sum();
    database
        .create_session(&NewSession::new("session-a", schema()), None, latest_bytes)
        .await
        .expect("session should be created");
    database
        .append_turn("session-a", &older_turn)
        .await
        .expect("older turn should be appended");
    database
        .append_turn("session-a", &latest_turn)
        .await
        .expect("latest turn should be appended");

    // Act
    let loaded = database
        .load_session("session-a")
        .await
        .expect("session should load");

    // Assert
    assert_eq!(loaded.turns, vec![latest_turn]);
}

#[tokio::test]
async fn database_paginates_turn_sizes_until_the_history_budget_is_filled() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");
    let turns = (0..70)
        .map(|position| {
            turn(
                &format!("question {position}"),
                &format!("answer {position}"),
            )
        })
        .collect::<Vec<_>>();
    let expected_turns = turns[5..].to_vec();
    let max_history_bytes = expected_turns
        .iter()
        .flatten()
        .map(ModelMessage::retained_bytes)
        .sum();
    database
        .create_session(
            &NewSession::new("session-a", schema()),
            None,
            max_history_bytes,
        )
        .await
        .expect("session should be created");
    for messages in &turns {
        database
            .append_turn("session-a", messages)
            .await
            .expect("turn should be appended");
    }

    // Act
    let loaded = database
        .load_session("session-a")
        .await
        .expect("session should load");

    // Assert
    assert_eq!(loaded.turns, expected_turns);
}

#[tokio::test]
async fn history_size_page_preserves_integer_boundary_turns() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");
    database
        .create_session(&NewSession::new("session-a", schema()), None, 100_000)
        .await
        .expect("session should be created");
    for (turn_position, retained_bytes) in [(i64::MIN, 1_i64), (i64::MAX, 2_i64)] {
        sqlx::query(
            r"
INSERT INTO session_turn (
    session_id, turn_position, status, error_type, lease_expires_at, created_at, updated_at
)
VALUES (?, ?, 'completed', NULL, NULL, 1, 1)
",
        )
        .bind("session-a")
        .bind(turn_position)
        .execute(&database.pool)
        .await
        .expect("boundary turn should be inserted");
        sqlx::query(
            r#"
INSERT INTO session_message (
    session_id, turn_position, message_position, kind, payload, retained_bytes, created_at
)
VALUES (?, ?, 0, 'user', '"boundary"', ?, 1)
"#,
        )
        .bind("session-a")
        .bind(turn_position)
        .bind(retained_bytes)
        .execute(&database.pool)
        .await
        .expect("boundary message should be inserted");
    }

    // Act
    let mut connection = database
        .pool
        .acquire()
        .await
        .expect("database connection should be acquired");
    let initial_page = load_turn_size_page(&mut connection, "session-a", None)
        .await
        .expect("initial page should load");
    let before_maximum = load_turn_size_page(&mut connection, "session-a", Some(i64::MAX))
        .await
        .expect("page before maximum should load");
    let before_minimum = load_turn_size_page(&mut connection, "session-a", Some(i64::MIN))
        .await
        .expect("page before minimum should load");

    // Assert
    assert_eq!(initial_page, [(i64::MAX, 2), (i64::MIN, 1)]);
    assert_eq!(before_maximum, [(i64::MIN, 1)]);
    assert_eq!(before_minimum, [] as [(i64, i64); 0]);
}

#[tokio::test]
async fn database_excludes_a_newest_turn_larger_than_the_budget() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");
    database
        .create_session(&NewSession::new("session-a", schema()), None, 1)
        .await
        .expect("session should be created");
    database
        .append_turn("session-a", &turn("question", "answer"))
        .await
        .expect("turn should be appended");

    // Act
    let loaded = database
        .load_session("session-a")
        .await
        .expect("session should load");

    // Assert
    assert_eq!(loaded.turns, Vec::<Vec<ModelMessage>>::new());
}

#[tokio::test]
async fn append_turn_rolls_back_every_message_when_one_insert_fails() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");
    database
        .create_session(&NewSession::new("session-a", schema()), None, 100_000)
        .await
        .expect("session should be created");
    sqlx::query(
        r"
CREATE TRIGGER reject_assistant_message
BEFORE INSERT ON session_message
WHEN NEW.kind = 'assistant'
BEGIN
    SELECT RAISE(ABORT, 'assistant rejected');
END
",
    )
    .execute(&database.pool)
    .await
    .expect("trigger should be created");

    // Act
    let error = database
        .append_turn("session-a", &turn("question", "answer"))
        .await
        .expect_err("turn append should fail");
    let message_count =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM session_message WHERE session_id = ?")
            .bind("session-a")
            .fetch_one(&database.pool)
            .await
            .expect("message count should load");

    // Assert
    assert!(matches!(error, SessionError::QueryContext { .. }));
    assert_eq!(message_count, 0);
}

#[tokio::test]
async fn on_disk_database_serializes_concurrent_appends_before_reading_positions() {
    // Arrange
    let temp_dir = tempdir().expect("temp directory should be created");
    let database_path = temp_dir.path().join("harness.db");
    let next_timestamp = Arc::new(AtomicI64::new(1));
    let timestamp_source: Arc<dyn TimestampSource> = {
        let next_timestamp = Arc::clone(&next_timestamp);

        Arc::new(move || next_timestamp.fetch_add(1, Ordering::SeqCst))
    };
    let database = Database::open_with_timestamp_source(&database_path, timestamp_source)
        .await
        .expect("database should open");
    for session_id in ["session-a", "session-b"] {
        database
            .create_session(&NewSession::new(session_id, schema()), None, 100_000)
            .await
            .expect("session should be created");
    }
    sqlx::query(
        r"
CREATE TRIGGER require_session_update_before_message
BEFORE INSERT ON session_message
WHEN (
    SELECT updated_at
    FROM session
    WHERE id = NEW.session_id
) != NEW.created_at
BEGIN
    SELECT RAISE(ABORT, 'session update must acquire the writer lock first');
END
",
    )
    .execute(&database.pool)
    .await
    .expect("ordering trigger should be created");
    let first_turn = turn("first question", "first answer");
    let second_turn = turn("second question", "second answer");

    // Act
    let (first_result, second_result) = tokio::join!(
        database.append_turn("session-a", &first_turn),
        database.append_turn("session-b", &second_turn),
    );

    // Assert
    first_result.expect("first concurrent append should succeed");
    second_result.expect("second concurrent append should succeed");
    let message_count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM session_message")
        .fetch_one(&database.pool)
        .await
        .expect("message count should load");
    assert_eq!(message_count, 4);
}

#[tokio::test]
async fn appending_an_empty_turn_to_a_missing_session_reports_not_found() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");

    // Act
    let error = database
        .append_turn("missing", &[])
        .await
        .expect_err("missing session should fail");

    // Assert
    assert!(matches!(error, SessionError::NotFound { .. }));
}
