//! Sessions stored before the default output mode became optional keep
//! working: `tests/fixtures/session_v13.sqlite` holds sessions written at
//! schema version 13, with completed and failed turns.

use std::error::Error;
use std::path::Path;
use std::sync::{Arc, Mutex};

use ag_harness::model::{ModelCompletion, ModelMessage, ModelRequest, ModelResponse};
use ag_harness::{Harness, Model, ModelError, OutputMode, OutputSchema, RunOutput, RunRequest};
use async_trait::async_trait;
use serde_json::{Value, json};
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
        let output = match request.output() {
            OutputMode::Text => Value::String(answer),
            _ => json!({ "answer": answer }),
        };
        self.requests
            .lock()
            .map_err(|_| ModelError::InvalidResponse)?
            .push(request);

        Ok(ModelCompletion::from_response(ModelResponse::Output(
            output,
        )))
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

    // Assert
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
