//! Public contract of one shareable `Harness`: clones run many durable
//! sessions concurrently against one SQLite file or one `MemoryStore`.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_harness::model::{ModelCompletion, ModelMessage, ModelProfile, ModelRequest, ModelResponse};
use ag_harness::store::MemoryStore;
use ag_harness::{Harness, HarnessError, Model, ModelError, OutputSchema, Session, SessionError};
use async_trait::async_trait;
use serde_json::json;
use sqlx::Connection;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use tokio::sync::oneshot;
use tokio::task::JoinSet;

use crate::model_profile_fixture::unbounded_model_profile;

/// Concurrent sessions per scenario.
const SESSIONS: usize = 6;
/// Turns each session sends before the final resumed turn.
const TURNS: u64 = 3;

/// Answers with the number of user messages it received, so a reply shows
/// how much of its own session's history each request replayed.
struct UserTurnCountingModel;

#[async_trait]
impl Model for UserTurnCountingModel {
    fn profile(&self) -> Option<ModelProfile> {
        Some(unbounded_model_profile())
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        let user_turns = request
            .messages()
            .iter()
            .filter(|message| matches!(message, ModelMessage::User(_)))
            .count();

        Ok(ModelCompletion::from_response(ModelResponse::Output(
            json!({ "user_turns": user_turns }),
        )))
    }
}

/// Takes the database write lock on a separate connection during its first
/// request and hands that connection to the test, so the harness must
/// persist the response while another writer holds the lock.
struct WriteLockingModel {
    lock_holder: Mutex<Option<oneshot::Sender<SqliteConnection>>>,
    path: PathBuf,
}

#[async_trait]
impl Model for WriteLockingModel {
    fn profile(&self) -> Option<ModelProfile> {
        Some(unbounded_model_profile())
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        let lock_holder = self.lock_holder.lock().expect("lock holder").take();
        if let Some(lock_holder) = lock_holder {
            let mut connection = raw_connection(&self.path).await;
            sqlx::raw_sql("BEGIN IMMEDIATE")
                .execute(&mut connection)
                .await
                .expect("write lock");
            lock_holder.send(connection).expect("test awaits the lock");
        }

        UserTurnCountingModel.complete(request).await
    }
}

/// Opens a connection to `path` outside every harness pool.
async fn raw_connection(path: &Path) -> SqliteConnection {
    SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(path))
        .await
        .expect("raw connection")
}

fn schema() -> Result<OutputSchema, Box<dyn Error>> {
    Ok(OutputSchema::new(json!({
        "type": "object",
        "properties": { "user_turns": { "type": "integer" } },
        "required": ["user_turns"],
        "additionalProperties": false,
    }))?)
}

fn sqlite_harness(path: &Path) -> Result<Harness, HarnessError> {
    Harness::builder()
        .model(Arc::new(UserTurnCountingModel))
        .sqlite(path)
        .build()
}

/// Creates `SESSIONS` sessions from clones of `harness`, each on its own
/// task, and sends `TURNS` turns in each, returning every session's last
/// reply.
async fn run_concurrent_sessions(harness: &Harness) -> Result<Vec<u64>, Box<dyn Error>> {
    let schema = schema()?;
    let mut tasks = JoinSet::new();
    for index in 0..SESSIONS {
        let harness = harness.clone();
        let schema = schema.clone();
        tasks.spawn(async move {
            let mut session = harness
                .session(format!("session-{index}"), schema)
                .create()
                .await?;

            send_turns(&mut session).await
        });
    }
    let mut replies = Vec::with_capacity(SESSIONS);
    while let Some(result) = tasks.join_next().await {
        replies.push(result??);
    }

    Ok(replies)
}

/// Sends `TURNS` turns in `session`, returning its last reply.
async fn send_turns(session: &mut Session) -> Result<u64, SessionError> {
    let mut last = 0;
    for turn in 0..TURNS {
        let outcome = session.send(format!("turn {turn}")).await?;
        last = outcome.output()["user_turns"].as_u64().unwrap_or_default();
    }

    Ok(last)
}

/// Resumes every session through `harness` and sends one more turn,
/// returning each reply.
async fn resume_every_session(harness: &Harness) -> Result<Vec<u64>, Box<dyn Error>> {
    let mut replies = Vec::with_capacity(SESSIONS);
    for index in 0..SESSIONS {
        let mut session = harness.resume(&format!("session-{index}")).await?;
        let outcome = session.send("resumed").await?;
        replies.push(outcome.output()["user_turns"].as_u64().unwrap_or_default());
    }

    Ok(replies)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clones_of_one_harness_run_concurrent_sessions_on_one_sqlite_file()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("sessions.db");
    // A second harness owns a second connection pool on the same file, so
    // its writes contend with the shared pool's. It creates the file first,
    // as the README asks of hosts: concurrent first opens of a new file can
    // race on schema migration.
    let mut independent = sqlite_harness(&path)?
        .session("independent", schema()?)
        .create()
        .await?;
    let shared = sqlite_harness(&path)?;

    // Act
    let (shared_replies, independent_replies) = tokio::join!(
        run_concurrent_sessions(&shared),
        send_turns(&mut independent),
    );
    let (shared_replies, independent_replies) = (shared_replies?, independent_replies?);
    let resumed = resume_every_session(&sqlite_harness(&path)?).await?;

    // Assert
    assert_eq!(shared_replies, vec![TURNS; SESSIONS]);
    assert_eq!(independent_replies, TURNS);
    assert_eq!(resumed, vec![TURNS + 1; SESSIONS]);

    Ok(())
}

#[tokio::test]
async fn turns_commit_while_another_connection_holds_a_read_transaction()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("sessions.db");
    let harness = sqlite_harness(&path)?;
    let mut session = harness.session("read-locked", schema()?).create().await?;
    let mut reader = raw_connection(&path).await;
    sqlx::raw_sql("BEGIN; SELECT count(*) FROM sqlite_master;")
        .execute(&mut reader)
        .await?;

    // Act
    // Under a rollback journal the open read transaction blocks every
    // commit until the busy timeout expires; WAL lets the turn commit.
    let reply = send_turns(&mut session).await;
    sqlx::raw_sql("COMMIT").execute(&mut reader).await?;

    // Assert
    assert_eq!(reply?, TURNS);

    Ok(())
}

#[tokio::test]
async fn turn_persistence_waits_for_another_writer_to_release_its_lock()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("sessions.db");
    let (lock_holder, locked) = oneshot::channel();
    let harness = Harness::builder()
        .model(Arc::new(WriteLockingModel {
            lock_holder: Mutex::new(Some(lock_holder)),
            path: path.clone(),
        }))
        .sqlite(&path)
        .build()?;
    let mut session = harness.session("write-locked", schema()?).create().await?;

    // Act
    // The model takes the write lock before it answers, so persisting the
    // answer starts while the lock is held; the delay before releasing it
    // only keeps the overlap, well inside the harness's busy timeout.
    let turn = tokio::spawn(async move { session.send("locked").await });
    let mut writer = locked.await?;
    tokio::time::sleep(Duration::from_millis(100)).await;
    sqlx::raw_sql("COMMIT").execute(&mut writer).await?;
    let outcome = turn.await??;
    let resumed = harness.resume("write-locked").await?.send("again").await?;

    // Assert
    assert_eq!(outcome.output()["user_turns"], 1);
    assert_eq!(resumed.output()["user_turns"], 2);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clones_of_one_harness_run_concurrent_sessions_on_one_memory_store()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let harness = Harness::builder()
        .model(Arc::new(UserTurnCountingModel))
        .store(Arc::new(MemoryStore::new()))
        .build()?;

    // Act
    let replies = run_concurrent_sessions(&harness).await?;
    let resumed = resume_every_session(&harness).await?;

    // Assert
    assert_eq!(replies, vec![TURNS; SESSIONS]);
    assert_eq!(resumed, vec![TURNS + 1; SESSIONS]);

    Ok(())
}
