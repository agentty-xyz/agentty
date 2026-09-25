use std::path::Path;
use std::sync::Arc;

use crate::TurnInput;
use crate::session::{Database, NewSession};
use crate::store::{SessionStore, WriteStatus};
use crate::store_conformance_test::{options, schema};

async fn create_session(database: &Database) {
    database
        .create_session(&NewSession::new("session", schema()), None, 4096)
        .await
        .expect("session");
}

#[tokio::test]
async fn journal_handle_retains_temporary_database_and_turn_ownership() {
    // Arrange
    let database = Database::open_in_memory().await.expect("database");
    create_session(&database).await;
    let mut guard = database
        .begin_turn(
            Arc::new(database.clone()),
            "session",
            &TurnInput::from("write"),
            &options(),
            0,
        )
        .await
        .expect("turn")
        .guard;
    let journal = guard.write_journal();
    let renewal_task = guard.renewal_task.take().expect("renewal task");
    guard.disarm();
    renewal_task.await.expect("renewal stopped");
    drop(guard);
    drop(database);

    // Act
    let id = journal
        .intent("retained", Path::new("repo"), "file.txt", None, b"new")
        .await
        .expect("journal retains its temporary database and owner");
    journal.finish(id, true).await.expect("persist outcome");
    let records = journal
        .database
        .load_writes("session")
        .await
        .expect("records");

    // Assert
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, id);
    assert_eq!(records[0].call_id, "retained");
    assert_eq!(records[0].status, WriteStatus::Applied);
    assert_eq!(records[0].turn_position, 0);
}
