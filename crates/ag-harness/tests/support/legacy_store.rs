//! Sessions stored before the default output mode became optional keep
//! working.
//!
//! `tests/fixtures/session_v13.sqlite` was written by `ag-harness` at
//! migration 13 (commit `1427a767`, before migration 014) with a scripted
//! model answering `{"answer": "answer to <prompt>"}` under the schema in
//! [`answer_schema`]:
//!
//! - session `legacy`, system prompt `Answer briefly.`: completed turns `first`
//!   and `second`, then turn `fail`, failed by a provider error;
//! - session `legacy-plain`, no system prompt: completed turn `only`.
//!
//! To rebuild it, check out that commit, create the two sessions against a
//! fresh database file with that model, and commit the file; the test below
//! rejects a fixture at any other migration version.

use std::error::Error;
use std::path::Path;
use std::sync::{Arc, Mutex};

use ag_harness::model::{ModelCompletion, ModelMessage, ModelRequest, ModelResponse};
use ag_harness::{Harness, Model, ModelError, OutputMode, OutputSchema, RunOutput, RunRequest};
use async_trait::async_trait;
use serde_json::json;
use sqlx::{Connection, SqliteConnection};
use tempfile::tempdir;

use crate::context_budget_fixture::unbounded_context_budget;

/// Answers JSON requests the way the fixture's turns were answered, and
/// text requests in text, recording each request.
struct AnsweringModel {
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

#[async_trait]
impl Model for AnsweringModel {
    fn supports_text_output(&self) -> bool {
        true
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        let answer = format!("answer to {}", request.prompt());
        let response = match request.output() {
            OutputMode::Text => ModelResponse::Text(answer),
            _ => ModelResponse::Output(json!({ "answer": answer })),
        };
        self.requests
            .lock()
            .map_err(|_| ModelError::InvalidResponse)?
            .push(request);

        Ok(ModelCompletion::from_response(response))
    }
}

fn answer_schema() -> Result<OutputSchema, Box<dyn Error>> {
    Ok(OutputSchema::new(json!({
        "type": "object",
        "properties": {"answer": {"type": "string"}},
        "required": ["answer"],
        "additionalProperties": false
    }))?)
}

/// Returns the latest applied migration of the database at `path`.
async fn migration_version(path: &Path) -> Result<i64, Box<dyn Error>> {
    let mut connection = SqliteConnection::connect(&format!("sqlite://{}", path.display())).await?;
    let version = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&mut connection)
        .await?;
    connection.close().await?;

    Ok(version)
}

/// Returns whether `session.output_schema` is declared `NOT NULL` in the
/// database at `path`.
async fn output_schema_required(path: &Path) -> Result<bool, Box<dyn Error>> {
    let mut connection = SqliteConnection::connect(&format!("sqlite://{}", path.display())).await?;
    let required: i64 = sqlx::query_scalar(
        "SELECT \"notnull\" FROM pragma_table_info('session') WHERE name = 'output_schema'",
    )
    .fetch_one(&mut connection)
    .await?;
    connection.close().await?;

    Ok(required != 0)
}

#[tokio::test]
async fn version_13_sessions_keep_their_schema_history_and_accept_text_runs()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempdir()?;
    let database = directory.path().join("sessions.sqlite");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/session_v13.sqlite"),
        &database,
    )?;
    assert_eq!(migration_version(&database).await?, 13, "fixture version");
    assert!(output_schema_required(&database).await?);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let harness = Harness::new(
        AnsweringModel {
            requests: Arc::clone(&requests),
        },
        unbounded_context_budget(),
    )
    .database(&database);

    // Act
    let legacy = harness.resume("legacy").await?;
    let structured = legacy.run(RunRequest::new("third")).await;
    let text = legacy
        .run(RunRequest::new("fourth").output(OutputMode::Text))
        .await;
    let plain = harness.resume("legacy-plain").await?;
    let other = plain.run(RunRequest::new("again")).await;
    drop((legacy, plain));
    drop(harness);

    // Assert
    assert_eq!(migration_version(&database).await?, 15);
    assert!(!output_schema_required(&database).await?);
    assert_eq!(
        structured.result?,
        RunOutput::Json(json!({"answer": "answer to third"}))
    );
    assert_eq!(text.result?, RunOutput::Text("answer to fourth".into()));
    assert_eq!(
        other.result?,
        RunOutput::Json(json!({"answer": "answer to again"}))
    );
    let requests = requests.lock().map_err(|_| "requests")?;
    assert_eq!(requests[0].schema(), Some(&answer_schema()?));
    assert_eq!(
        requests[0].messages()[..5],
        [
            ModelMessage::System("Answer briefly.".into()),
            ModelMessage::User("first".into()),
            ModelMessage::Assistant(r#"{"answer":"answer to first"}"#.into()),
            ModelMessage::User("second".into()),
            ModelMessage::Assistant(r#"{"answer":"answer to second"}"#.into()),
        ]
    );
    assert!(
        requests[0]
            .messages()
            .contains(&ModelMessage::User("fail".into())),
        "the failed legacy turn replays with its stop note"
    );
    assert_eq!(
        requests[2].messages()[..2],
        [
            ModelMessage::User("only".into()),
            ModelMessage::Assistant(r#"{"answer":"answer to only"}"#.into()),
        ]
    );

    Ok(())
}
