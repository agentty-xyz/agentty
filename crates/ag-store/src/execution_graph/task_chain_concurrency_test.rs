//! Writer ownership must precede workflow eligibility snapshots in WAL mode.

use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use ag_session::TaskChainPhase;
use sqlx::SqlitePool;
use sqlx::error::DatabaseError;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use tokio::sync::oneshot;

use super::{chain_graph, claim};
use crate::Database;

#[tokio::test]
async fn workflow_writes_lock_before_reads_during_concurrent_wal_commits() {
    for cancel in [false, true] {
        // Arrange: use separate on-disk connections; in-memory databases do
        // not reproduce WAL snapshot promotion failures.
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("workflow.sqlite");
        let database = Database::open(&path).await.expect("database");
        let project = database
            .projects()
            .upsert_project("project", None)
            .await
            .expect("project");
        database
            .sessions()
            .insert_session("session", "gpt-6.1-sol", "main", "Review", project)
            .await
            .expect("session");
        database
            .graphs()
            .start_task_chain(&chain_graph("chain"), "Add search")
            .await
            .expect("existing graph");
        let planner = if cancel {
            Some(claim(&database, "planner").await)
        } else {
            None
        };
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .journal_mode(SqliteJournalMode::Wal)
                    .foreign_keys(true),
            )
            .await
            .expect("subject pool");
        let subject = Database::from_pool(pool.clone(), Arc::new(|| 123));
        let (entered_rx, release_tx) = pause_eligibility(&pool, cancel).await;
        let mut writer = database.pool().acquire().await.expect("writer");
        sqlx::query("PRAGMA busy_timeout = 0")
            .execute(&mut *writer)
            .await
            .expect("do not wait for subject");

        // Act: pause the eligibility access and try a concurrent WAL commit.
        let task = tokio::spawn(async move {
            if let Some(planner) = planner {
                subject
                    .graphs()
                    .cancel_execution(&planner.record.execution)
                    .await
            } else {
                subject
                    .graphs()
                    .start_task_chain(&chain_graph("new-chain"), "Add search")
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(5), entered_rx)
            .await
            .expect("eligibility guard")
            .expect("eligibility reached");
        let concurrent =
            sqlx::query("UPDATE session SET title = 'Concurrent update' WHERE id = 'session'")
                .execute(&mut *writer)
                .await;
        release_tx.send(()).expect("release eligibility");
        let result = task.await.expect("subject task");

        // Assert: the subject already owns the writer lock, so the other
        // connection must wait until it commits instead of invalidating reads.
        assert!(result.expect("workflow write must not fail with BUSY_SNAPSHOT"));
        let error = concurrent.expect_err("writer must be excluded before eligibility reads");
        assert_eq!(
            error
                .as_database_error()
                .and_then(DatabaseError::code)
                .as_deref(),
            Some("5")
        );
        sqlx::query("UPDATE session SET title = 'Concurrent update' WHERE id = 'session'")
            .execute(&mut *writer)
            .await
            .expect("writer succeeds after workflow commit");
        if cancel {
            let chain = database
                .graphs()
                .load_task_chain("chain")
                .await
                .expect("chain")
                .expect("workflow");
            assert_eq!(chain.phase, TaskChainPhase::Canceled);
        } else {
            assert!(
                database
                    .graphs()
                    .load_task_chain("new-chain")
                    .await
                    .expect("chain")
                    .is_some()
            );
        }
        drop(writer);
        pool.close().await;
        database.pool().close().await;
    }
}

/// Pauses one eligibility SELECT through a connection-local view without
/// changing production code or the on-disk schema.
async fn pause_eligibility(
    pool: &SqlitePool,
    cancel: bool,
) -> (oneshot::Receiver<()>, mpsc::Sender<()>) {
    let (entered_tx, entered_rx) = oneshot::channel();
    let entered_tx = Mutex::new(Some(entered_tx));
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let mut connection = pool.acquire().await.expect("connection");
    connection
        .lock_handle()
        .await
        .expect("handle")
        .create_collation("eligibility", move |left, right| {
            let sender = entered_tx.lock().expect("entered sender").take();
            if let Some(sender) = sender {
                let _ = sender.send(());
                release_rx
                    .lock()
                    .expect("release receiver")
                    .recv_timeout(Duration::from_secs(5))
                    .expect("release guard");
            }

            left.cmp(right)
        })
        .expect("collation barrier");
    let view = if cancel {
        "CREATE TEMP VIEW execution_graph_execution AS SELECT * FROM \
         main.execution_graph_execution WHERE 'pause' COLLATE eligibility = 'pause'"
    } else {
        "CREATE TEMP VIEW task AS SELECT * FROM main.task WHERE 'pause' COLLATE eligibility = \
         'pause'"
    };
    sqlx::query(view)
        .execute(&mut *connection)
        .await
        .expect("eligibility view");

    (entered_rx, release_tx)
}
