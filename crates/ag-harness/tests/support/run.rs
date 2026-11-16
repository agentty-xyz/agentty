//! Public run contract through the crate root: requests, outcomes, usage,
//! parsing, failure classification, and handles.

use std::error::Error;
use std::io;
use std::sync::{Arc, Mutex};

use ag_harness::bash::BashConfig;
use ag_harness::model::{
    CompletionMetadata, CompletionUsage, ModelCompletion, ModelMessage, ModelRequest, ModelResponse,
};
use ag_harness::store::MemoryStore;
use ag_harness::tool::ToolCall;
use ag_harness::{
    CancelToken, ErrorKind, Harness, Model, ModelError, OutputSchema, Run, RunHandle, RunOutcome,
    RunOutput, RunRequest, Tool, ToolPolicy, Usage,
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::context_budget_fixture::unbounded_context_budget;
use crate::repository_fixture::repository_with_host_git;
use crate::{BashDrivingModel, ScriptedExecutor};

/// Compiles only while runs can move into spawned tasks and their
/// cancellation can be shared across them.
const _: fn() = || {
    fn assert_send<T: Send + 'static>() {}
    fn assert_shareable<T: Clone + Send + Sync + 'static>() {}
    assert_send::<Run>();
    assert_send::<RunHandle>();
    assert_send::<RunOutcome>();
    assert_shareable::<CancelToken>();
    assert_shareable::<RunRequest>();
};

#[derive(Debug, Deserialize, PartialEq)]
struct Named {
    name: String,
}

/// Answers the first request with a denied `read` call and the next with
/// output, or with a rate limit for the prompt `fail`, reporting usage on
/// every completion.
struct ToolRoundModel;

#[async_trait]
impl Model for ToolRoundModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        if !matches!(
            request.messages().last(),
            Some(ModelMessage::ToolResult { .. })
        ) {
            let call =
                ToolCall::from_json("read-name".into(), "read", r#"{"path":"name.txt"}"#, None)?;

            return Ok(completion(
                ModelResponse::ToolCall(call),
                CompletionUsage::new(Some(40), Some(60), Some(100), Some(10), Some(4), Some(110)),
            ));
        }
        if request.prompt() == "fail" {
            return Err(ModelError::provider(
                429,
                None,
                io::Error::other("slow down"),
            ));
        }

        Ok(completion(
            ModelResponse::Output(json!({"name": "Ada"})),
            CompletionUsage::new(Some(90), Some(60), Some(150), Some(20), Some(6), Some(170)),
        ))
    }
}

fn completion(response: ModelResponse, usage: CompletionUsage) -> ModelCompletion {
    ModelCompletion::new(
        CompletionMetadata::new("stop".into(), None, None, None, Some(usage)),
        response,
    )
}

fn name_schema() -> Result<OutputSchema, Box<dyn Error>> {
    Ok(OutputSchema::new(json!({
        "type": "object",
        "properties": {"name": {"type": "string"}},
        "required": ["name"]
    }))?)
}

fn usage_counts(usage: Usage) -> [u64; 5] {
    [
        usage.input,
        usage.output,
        usage.cache_read,
        usage.cache_write,
        usage.reasoning,
    ]
}

#[tokio::test]
async fn one_shot_run_sums_usage_over_every_model_request() -> Result<(), Box<dyn Error>> {
    // Arrange
    let harness = Harness::new(ToolRoundModel, unbounded_context_budget());
    let request = RunRequest::new("Read the name").output(name_schema()?);

    // Act
    let outcome = harness.run(request).await;

    // Assert
    let output = outcome.result?;
    assert_eq!(output, RunOutput::Json(json!({"name": "Ada"})));
    assert_eq!(output.parse::<Named>()?, Named { name: "Ada".into() });
    assert_eq!(outcome.report.model_requests().len(), 2);
    assert_eq!(usage_counts(outcome.usage), [250, 30, 130, 0, 10]);
    assert_eq!(outcome.files_changed, []);
    assert_eq!(outcome.commands, []);

    Ok(())
}

#[test]
fn run_output_parses_text_as_a_string_and_rejects_mismatches() {
    // Arrange
    let text = RunOutput::Text("plain".into());
    let json = RunOutput::Json(json!({"name": 7}));

    // Act
    let parsed = text.parse::<String>();
    let mismatch = json.parse::<Named>();

    // Assert
    assert_eq!(parsed.ok().as_deref(), Some("plain"));
    assert_eq!(
        mismatch.map_err(|error| error.kind()).err(),
        Some(ErrorKind::InvalidOutput)
    );
}

#[tokio::test]
async fn one_shot_run_without_an_output_schema_fails_as_config() {
    // Arrange
    let harness = Harness::new(ToolRoundModel, unbounded_context_budget());

    // Act
    let outcome = harness.run(RunRequest::new("Read the name")).await;

    // Assert
    let error = outcome.result.expect_err("a one-shot run needs a schema");
    assert_eq!(error.kind(), ErrorKind::Config);
    assert!(error.to_string().contains("RunRequest::output"));
    assert_eq!(outcome.report.model_requests(), []);
    assert_eq!(outcome.usage, Usage::default());
}

#[tokio::test]
async fn failed_session_run_classifies_the_error_and_keeps_earlier_usage()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let harness = Harness::new(ToolRoundModel, unbounded_context_budget())
        .store(Arc::new(MemoryStore::new()));
    let session = harness.session("run", name_schema()?).create().await?;

    // Act
    let outcome = session.run(RunRequest::new("fail")).await;

    // Assert
    let error = outcome.result.expect_err("the rate limit fails the run");
    assert_eq!(error.kind(), ErrorKind::RateLimited);
    assert!(error.is_retryable());
    assert_eq!(outcome.report.model_requests().len(), 1);
    assert_eq!(usage_counts(outcome.usage), [100, 10, 40, 0, 4]);

    Ok(())
}

#[tokio::test]
async fn cancelled_handle_reports_a_cancelled_run() -> Result<(), Box<dyn Error>> {
    // Arrange
    let harness = Harness::new(ToolRoundModel, unbounded_context_budget());
    let handle = harness
        .run(RunRequest::new("Read the name").output(name_schema()?))
        .into_handle();

    // Act
    handle.cancel();
    let outcome = handle.outcome().await;

    // Assert
    let error = outcome.result.expect_err("the run was cancelled");
    assert_eq!(error.kind(), ErrorKind::Cancelled);
    assert_eq!(outcome.report.model_requests(), []);

    Ok(())
}

#[tokio::test]
async fn one_shot_run_lists_its_bash_commands() -> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempfile::tempdir()?;
    let root = directory.path().canonicalize()?;
    let configuration = BashConfig::for_executor(
        Arc::new(ScriptedExecutor {
            workspace: root.clone(),
        }),
        "/bin/container-bash".into(),
        "scripted-runtime-1".into(),
        std::time::Duration::from_secs(5),
        256,
    )?
    .with_host_information();
    let harness = Harness::new(
        BashDrivingModel {
            advertised: Arc::new(Mutex::new(Vec::new())),
        },
        unbounded_context_budget(),
    )
    .repository(repository_with_host_git(&root));
    let request = RunRequest::new("printf scripted")
        .output(OutputSchema::new(json!({"type": "object"}))?)
        .tool_policy(ToolPolicy::default().allow(Tool::Bash))
        .bash(configuration);

    // Act
    let outcome = harness.run(request).await;

    // Assert
    let output = outcome.result?.parse::<serde_json::Value>()?;
    assert_eq!(output["stdout"], "scripted");
    assert_eq!(outcome.commands.len(), 1);
    assert_eq!(outcome.commands[0].intent.command, "printf scripted");
    assert!(outcome.commands[0].outcome.is_some());

    Ok(())
}
