use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use tempfile::tempdir;

use super::support::{create_version_one_database, read_call, schema, turn, turn_options};
use crate::context::{HistoryProjection, HistoryTurn};
use crate::input::TurnInput;
use crate::model::ModelMessage;
use crate::session::{
    Database, NewSession, SessionError, TimestampSource, TurnSizeRow, load_turn_size_page,
};
use crate::store::{AcquiredTurn, SessionStore as _};

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
    assert_eq!(loaded.turns, vec![(1, HistoryTurn::from(latest_turn))]);
}

#[tokio::test]
async fn database_paginates_turn_sizes_until_the_history_outgrows_its_budget() {
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
    let bytes = |messages: &Vec<ModelMessage>| {
        messages
            .iter()
            .map(ModelMessage::retained_bytes)
            .sum::<usize>()
    };
    // The newest 65 turns fill the budget, so the 66th, on the second size
    // page, outgrows it.
    let max_history_bytes = turns[5..].iter().map(bytes).sum::<usize>();
    // Re-projection then keeps the newest turns that fit half the budget.
    let mut used = 0;
    let kept = turns
        .iter()
        .rev()
        .take_while(|messages| {
            used += bytes(messages);
            used <= max_history_bytes / 2
        })
        .count();
    let first_kept = turns.len() - kept;
    let expected_turns = turns[first_kept..].to_vec();
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
    assert!((30..35).contains(&kept), "{kept}");
    let first_kept = i64::try_from(first_kept).expect("position fits i64");
    assert_eq!(
        loaded.turns,
        (first_kept..)
            .zip(expected_turns.into_iter().map(HistoryTurn::from))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        loaded.projection,
        HistoryProjection::new(first_kept, first_kept, first_kept)
    );
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
    let initial_page = load_turn_size_page(&mut connection, "session-a", None, None)
        .await
        .expect("initial page should load");
    let before_maximum = load_turn_size_page(&mut connection, "session-a", Some(i64::MAX), None)
        .await
        .expect("page before maximum should load");
    let before_minimum = load_turn_size_page(&mut connection, "session-a", Some(i64::MIN), None)
        .await
        .expect("page before minimum should load");

    // Assert
    assert_eq!(sizes(&initial_page), [(i64::MAX, 2), (i64::MIN, 1)]);
    assert_eq!(sizes(&before_maximum), [(i64::MIN, 1)]);
    assert_eq!(sizes(&before_minimum), [] as [(i64, i64); 0]);
}

#[tokio::test]
async fn reprojection_reads_older_size_pages_for_turns_it_keeps_briefly() {
    // Arrange
    let database = Database::open_in_memory()
        .await
        .expect("database should open");
    let tool_turn = |position: usize, bytes: usize| {
        vec![
            ModelMessage::User(format!("question {position}")),
            ModelMessage::AssistantToolCall(read_call(&format!("call {position}"))),
            ModelMessage::ToolResult {
                call_id: format!("call {position}"),
                content: "x".repeat(bytes),
                name: "read".to_string(),
            },
            ModelMessage::Assistant(format!("answer {position}")),
        ]
    };
    // Seventy small tool turns, then one that alone outgrows the budget and
    // is too large to elide.
    let mut turns = (0..70)
        .map(|position| tool_turn(position, 40))
        .collect::<Vec<_>>();
    turns.push(tool_turn(70, 100_000));
    database
        .create_session(&NewSession::new("session-a", schema()), None, 10_000)
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
    // The oversized newest turn replays brief, so the size pages its first
    // pass never read hold every older turn, each brief as well.
    assert_eq!(loaded.turns.len(), turns.len());
    assert!(
        loaded
            .turns
            .iter()
            .all(|(_, turn)| turn.messages.len() == 2)
    );
    assert_eq!(loaded.projection, HistoryProjection::new(0, 71, 71));
}

#[tokio::test]
async fn sessions_stored_before_projections_start_whole_and_keep_their_derived_projection() {
    // Arrange
    let directory = tempdir().expect("temporary directory");
    let database_path = directory.path().join("legacy.db");
    create_version_one_database(&database_path).await;
    let database = Database::open(&database_path)
        .await
        .expect("migrated database");
    let whole = database
        .load_session("session-a")
        .await
        .expect("legacy session");
    // Both stored turns, at positions 3 and 8, retain 45 bytes together.
    sqlx::query("UPDATE session SET max_history_bytes = 30")
        .execute(database.pool())
        .await
        .expect("smaller budget");

    // Act
    let derived = database
        .load_session("session-a")
        .await
        .expect("outgrown session");
    let mut acquired = AcquiredTurn::begin(
        Arc::new(database.clone()),
        "session-a",
        &TurnInput::from("new"),
        &turn_options(),
        0,
    )
    .await
    .expect("new turn");
    acquired
        .guard
        .complete(&[], acquired.projection)
        .await
        .expect("completion");
    let stored: (i64, i64, i64) = sqlx::query_as(
        "SELECT history_start, history_brief_before, history_elided_before FROM session",
    )
    .fetch_one(database.pool())
    .await
    .expect("stored projection");

    // Assert
    assert_eq!(whole.projection, HistoryProjection::default());
    assert_eq!(
        whole
            .turns
            .iter()
            .map(|(position, _)| *position)
            .collect::<Vec<_>>(),
        [3, 8]
    );
    assert_eq!(derived.projection, HistoryProjection::new(8, 8, 8));
    assert_eq!(
        derived
            .turns
            .iter()
            .map(|(position, _)| *position)
            .collect::<Vec<_>>(),
        [8]
    );
    assert_eq!(acquired.projection, derived.projection);
    assert_eq!(stored, (8, 8, 8));
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
    assert_eq!(loaded.turns, Vec::<(i64, HistoryTurn)>::new());
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

fn sizes(rows: &[TurnSizeRow]) -> Vec<(i64, i64)> {
    rows.iter()
        .map(|row| (row.turn_position, row.retained_bytes))
        .collect()
}
