//! Public contract of one shareable `Harness`: clones run many durable
//! sessions concurrently against one SQLite file or one `MemoryStore`.

use std::error::Error;
use std::path::Path;
use std::sync::Arc;

use ag_harness::model::{ModelCompletion, ModelMessage, ModelProfile, ModelRequest, ModelResponse};
use ag_harness::store::MemoryStore;
use ag_harness::{Harness, HarnessError, Model, ModelError, OutputSchema, Session, SessionError};
use async_trait::async_trait;
use serde_json::json;
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
    // its writes contend with the shared pool's through WAL and the busy
    // timeout. It creates the file first: concurrent first opens from
    // separate pools may race on initialization.
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
