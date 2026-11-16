//! Per-run request options: host instructions after the cached prompt
//! prefix, plain-text output modes, and provider generation parameters.

use std::error::Error;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};

use ag_harness::model::{ModelCompletion, ModelMessage, ModelRequest, ModelResponse};
use ag_harness::recovery::ExecutionIdentity;
use ag_harness::store::MemoryStore;
use ag_harness::{
    ErrorKind, GenerationParams, Harness, Model, ModelError, OutputMode, OutputSchema, RunOutput,
    RunRequest, SessionError, ToolPolicy, TurnOptions,
};
use async_trait::async_trait;
use serde_json::json;
use tempfile::tempdir;

use crate::context_budget_fixture::unbounded_context_budget;

type Requests = Arc<Mutex<Vec<ModelRequest>>>;

/// Answers text requests with plain text naming the prompt and JSON
/// requests with a name, recording each request.
struct TextModel {
    requests: Requests,
}

#[async_trait]
impl Model for TextModel {
    fn supports_text_output(&self) -> bool {
        true
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        let response = match request.output() {
            OutputMode::Text => {
                ModelResponse::Text(format!("plain answer to {}", request.prompt()))
            }
            _ => ModelResponse::Output(json!({"name": "Ada"})),
        };
        self.requests
            .lock()
            .map_err(|_| ModelError::InvalidResponse)?
            .push(request);

        Ok(ModelCompletion::from_response(response))
    }
}

/// Answers in JSON only and rejects every output-token bound, recording
/// each request.
struct JsonOnlyModel {
    requests: Requests,
}

#[async_trait]
impl Model for JsonOnlyModel {
    fn validate_generation(&self, _generation: &GenerationParams) -> Result<(), ModelError> {
        Err(ModelError::UnsupportedGeneration {
            reason: "no output-token bound".to_string(),
        })
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        self.requests
            .lock()
            .map_err(|_| ModelError::InvalidResponse)?
            .push(request);

        Ok(ModelCompletion::from_response(ModelResponse::Output(
            json!({"name": "Ada"}),
        )))
    }
}

fn text_harness(requests: &Requests) -> Harness {
    Harness::new(
        TextModel {
            requests: Arc::clone(requests),
        },
        unbounded_context_budget(),
    )
}

fn name_schema() -> Result<OutputSchema, Box<dyn Error>> {
    Ok(OutputSchema::new(json!({
        "type": "object",
        "properties": {"name": {"type": "string"}},
        "required": ["name"]
    }))?)
}

fn recorded(requests: &Requests) -> Result<Vec<ModelRequest>, Box<dyn Error>> {
    Ok(requests.lock().map_err(|_| "requests")?.clone())
}

#[tokio::test]
async fn instructions_follow_the_cached_prefix_and_steer_only_their_own_run()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempdir()?;
    let requests = Requests::default();
    let harness = text_harness(&requests).database(directory.path().join("sessions.sqlite"));
    let session = harness
        .session("instructed", name_schema()?)
        .system_prompt("policy")
        .create()
        .await?;

    // Act
    let first = session
        .run(RunRequest::new("first").instructions("Answer tersely."))
        .await;
    let second = session
        .run(RunRequest::new("second").instructions("Answer in French."))
        .await;
    drop(session);
    let resumed = harness.resume("instructed").await?;
    let third = resumed.run(RunRequest::new("third")).await;

    // Assert
    for outcome in [first, second, third] {
        assert_eq!(outcome.result?, RunOutput::Json(json!({"name": "Ada"})));
    }
    let requests = recorded(&requests)?;
    let answer = ModelMessage::Assistant(r#"{"name":"Ada"}"#.into());
    assert_eq!(
        requests[0].messages(),
        [
            ModelMessage::System("policy".into()),
            ModelMessage::User("first".into()),
            ModelMessage::Instructions("Answer tersely.".into()),
        ]
    );
    assert_eq!(
        requests[1].messages(),
        [
            ModelMessage::System("policy".into()),
            ModelMessage::User("first".into()),
            answer.clone(),
            ModelMessage::User("second".into()),
            ModelMessage::Instructions("Answer in French.".into()),
        ],
        "the first run's instructions are not replayed"
    );
    assert_eq!(
        requests[2].messages(),
        [
            ModelMessage::System("policy".into()),
            ModelMessage::User("first".into()),
            answer.clone(),
            ModelMessage::User("second".into()),
            answer,
            ModelMessage::User("third".into()),
        ],
        "after resume, no earlier run's instructions apply"
    );

    Ok(())
}

#[tokio::test]
async fn text_output_overrides_the_json_default_for_one_run_only() -> Result<(), Box<dyn Error>> {
    // Arrange
    let requests = Requests::default();
    let harness = text_harness(&requests).store(Arc::new(MemoryStore::new()));
    let session = harness.session("mixed", name_schema()?).create().await?;

    // Act
    let text = session
        .run(RunRequest::new("first").output(OutputMode::Text))
        .await;
    let json = session.run(RunRequest::new("second")).await;

    // Assert
    let text = text.result?;
    assert_eq!(text, RunOutput::Text("plain answer to first".into()));
    assert_eq!(text.parse::<String>()?, "plain answer to first");
    assert_eq!(json.result?, RunOutput::Json(json!({"name": "Ada"})));
    let requests = recorded(&requests)?;
    assert_eq!(requests[0].output(), &OutputMode::Text);
    assert_eq!(requests[0].schema(), None);
    assert_eq!(requests[1].schema(), Some(&name_schema()?));
    assert!(
        requests[1]
            .messages()
            .contains(&ModelMessage::Assistant("plain answer to first".into())),
        "the text answer replays as plain assistant text"
    );

    Ok(())
}

#[tokio::test]
async fn text_default_sessions_answer_in_text_after_resume() -> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempdir()?;
    let requests = Requests::default();
    let harness = text_harness(&requests).database(directory.path().join("sessions.sqlite"));
    let session = harness.session("plain", OutputMode::Text).create().await?;

    // Act
    let created = session.run(RunRequest::new("first")).await;
    drop(session);
    let resumed = harness.resume("plain").await?;
    let reopened = resumed.run(RunRequest::new("second")).await;
    let structured = resumed
        .run(RunRequest::new("third").output(name_schema()?))
        .await;

    // Assert
    assert_eq!(
        created.result?,
        RunOutput::Text("plain answer to first".into())
    );
    assert_eq!(
        reopened.result?,
        RunOutput::Text("plain answer to second".into())
    );
    assert_eq!(structured.result?, RunOutput::Json(json!({"name": "Ada"})));
    let requests = recorded(&requests)?;
    assert_eq!(requests[1].output(), &OutputMode::Text);

    Ok(())
}

#[tokio::test]
async fn unsupported_request_options_fail_as_config_before_execution() -> Result<(), Box<dyn Error>>
{
    // Arrange
    let requests = Requests::default();
    let harness = Harness::new(
        JsonOnlyModel {
            requests: Arc::clone(&requests),
        },
        unbounded_context_budget(),
    )
    .store(Arc::new(MemoryStore::new()));
    let session = harness
        .session("json-only", name_schema()?)
        .create()
        .await?;
    let tokens = NonZeroU32::new(64).ok_or("tokens")?;

    // Act
    let text = session
        .run(RunRequest::new("first").output(OutputMode::Text))
        .await;
    let bounded = session
        .run(
            RunRequest::new("second")
                .generation(GenerationParams::default().with_max_output_tokens(tokens)),
        )
        .await;

    // Assert
    for outcome in [text, bounded] {
        let error = outcome.result.expect_err("the option is unsupported");
        assert_eq!(error.kind(), ErrorKind::Config);
        assert_eq!(outcome.report.model_requests(), []);
    }
    assert_eq!(recorded(&requests)?, []);

    Ok(())
}

#[tokio::test]
async fn one_shot_turns_carry_instructions_generation_and_text_output() -> Result<(), Box<dyn Error>>
{
    // Arrange
    let requests = Requests::default();
    let harness = text_harness(&requests);
    let tokens = NonZeroU32::new(128).ok_or("tokens")?;
    let options = TurnOptions::new(OutputMode::Text, ToolPolicy::default())
        .with_instructions("Answer tersely.")
        .with_generation(GenerationParams::default().with_max_output_tokens(tokens));

    // Act
    let outcome = harness.turn("first", options).await?;

    // Assert
    assert_eq!(outcome.output(), &json!("plain answer to first"));
    let requests = recorded(&requests)?;
    assert_eq!(
        requests[0].messages(),
        [
            ModelMessage::User("first".into()),
            ModelMessage::Instructions("Answer tersely.".into()),
        ]
    );
    assert_eq!(requests[0].generation().max_output_tokens, Some(tokens));

    Ok(())
}

#[tokio::test]
async fn generation_params_apply_to_their_run_only() -> Result<(), Box<dyn Error>> {
    // Arrange
    let requests = Requests::default();
    let harness = text_harness(&requests).store(Arc::new(MemoryStore::new()));
    let session = harness.session("bounded", name_schema()?).create().await?;
    let tokens = NonZeroU32::new(256).ok_or("tokens")?;

    // Act
    let bounded = session
        .run(
            RunRequest::new("first")
                .generation(GenerationParams::default().with_max_output_tokens(tokens)),
        )
        .await;
    let unbounded = session.run(RunRequest::new("second")).await;

    // Assert
    bounded.result?;
    unbounded.result?;
    let requests = recorded(&requests)?;
    assert_eq!(requests[0].generation().max_output_tokens, Some(tokens));
    assert_eq!(requests[1].generation(), GenerationParams::default());

    Ok(())
}

#[tokio::test]
async fn host_requests_fingerprint_instructions_and_generation() -> Result<(), Box<dyn Error>> {
    // Arrange
    let requests = Requests::default();
    let harness = text_harness(&requests)
        .execution_identity(ExecutionIdentity::new("text-model", "1")?)
        .store(Arc::new(MemoryStore::new()));
    let mut session = harness.session("hosted", name_schema()?).create().await?;
    let tokens = NonZeroU32::new(64).ok_or("tokens")?;
    let options = TurnOptions::new(name_schema()?, ToolPolicy::default())
        .with_instructions("Answer tersely.")
        .with_generation(GenerationParams::default().with_max_output_tokens(tokens));

    // Act
    let original = session
        .turn("first")
        .options(options.clone())
        .host_id("request")
        .await?;
    let retried = session
        .turn("first")
        .options(options)
        .host_id("request")
        .await?;
    let changed = session
        .turn("first")
        .options(
            TurnOptions::new(name_schema()?, ToolPolicy::default())
                .with_instructions("Answer in French."),
        )
        .host_id("request")
        .await;

    // Assert
    assert_eq!(retried.output(), original.output());
    assert!(matches!(changed, Err(SessionError::HostTurnConflict)));
    assert_eq!(recorded(&requests)?.len(), 1);

    Ok(())
}
