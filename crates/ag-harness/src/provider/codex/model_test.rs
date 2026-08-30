use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;

use super::super::client::{CodexResponsesClient, CodexResponsesRequest};
use super::super::sse::CodexResponsesCompletion;
use super::super::test_support::person_schema;
use super::{
    Codex, CodexBackend, CodexConfig, CodexPrompt, DEFAULT_INSTRUCTIONS, persisted_replay,
    validate_strict_output_schema, validate_strict_schema_node,
};
use crate::model::{
    GeneratedResponse, Model, ModelError, ModelMessage, ModelMetadataError, ModelRequest,
};
use crate::{Harness, OutputSchema, ReasoningEffort, ToolDefinition};

#[derive(Default)]
struct FixedClient {
    requests: Mutex<Vec<CodexResponsesRequest>>,
}

#[derive(Default)]
struct FormattedClient {
    requests: Mutex<Vec<CodexResponsesRequest>>,
}

#[async_trait]
impl CodexResponsesClient for FormattedClient {
    async fn complete(
        &self,
        request: CodexResponsesRequest,
    ) -> Result<CodexResponsesCompletion, ModelError> {
        self.requests
            .lock()
            .expect("request recorder should lock")
            .push(request);
        let output = "{\n  \"name\": \"Ada\"\n}";

        Ok(CodexResponsesCompletion {
            account_fingerprint: Some("account-fingerprint".to_string()),
            output: output.to_string(),
            reasoning_content: Some(
                json!([
                    {
                        "type": "reasoning",
                        "id": "reasoning-1",
                        "encrypted_content": "opaque"
                    },
                    {
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": output }]
                    }
                ])
                .to_string(),
            ),
            response_id: Some("response-1".to_string()),
            response_model: None,
            status: "completed".to_string(),
            usage: None,
        })
    }
}

#[async_trait]
impl CodexResponsesClient for FixedClient {
    async fn complete(
        &self,
        request: CodexResponsesRequest,
    ) -> Result<CodexResponsesCompletion, ModelError> {
        self.requests
            .lock()
            .expect("request recorder should lock")
            .push(request);

        Ok(CodexResponsesCompletion {
            account_fingerprint: Some("account-fingerprint".to_string()),
            output: r#"{"name":"Ada"}"#.to_string(),
            reasoning_content: Some(
                concat!(
                    "[{\"type\":\"reasoning\",\"id\":\"reasoning-1\",",
                    "\"encrypted_content\":\"opaque\"},{\"type\":\"message\",",
                    "\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",",
                    "\"text\":\"{\\\"name\\\":\\\"Ada\\\"}\"}]}]"
                )
                .to_string(),
            ),
            response_id: Some("response-1".to_string()),
            response_model: None,
            status: "completed".to_string(),
            usage: None,
        })
    }
}

#[test]
fn configuration_exposes_model_and_auth_override() {
    // Arrange
    let config = CodexConfig::new("gpt-test").auth_file("custom-auth.json");

    // Act
    let model = Codex::new(config.clone()).expect("Codex configuration should be valid");
    let invalid = Codex::new(CodexConfig::new(" ")).err();

    // Assert
    assert_eq!(config.model(), "gpt-test");
    assert_eq!(config.auth_file_path(), Some(Path::new("custom-auth.json")));
    assert_eq!(CodexConfig::new("gpt-test").auth_file_path(), None);
    assert_eq!(
        Model::metadata(&model)
            .expect("Codex metadata should be present")
            .model(),
        "gpt-test"
    );
    assert!(matches!(invalid, Some(ModelMetadataError::EmptyModel)));
}

#[tokio::test]
async fn backend_maps_messages_and_returns_structured_output() {
    // Arrange
    let client = Arc::new(FixedClient::default());
    let backend = CodexBackend::with_client("gpt-test", client.clone());
    let request = ModelRequest::with_history(
        vec![ModelMessage::System("Follow the schema".to_string())],
        "Extract the name",
        person_schema(),
    )
    .with_model_reasoning_effort(crate::ReasoningEffort::High);

    // Act
    let response = backend
        .generate(&request)
        .await
        .expect("fixed response should succeed");

    // Assert
    assert!(matches!(
        &response,
        GeneratedResponse::Output {
            metadata,
            output,
            reasoning_content,
            ..
        }
            if output == r#"{"name":"Ada"}"#
                && metadata.response_id() == Some("response-1")
                && metadata.response_model() == Some("gpt-test")
                && reasoning_content.is_some()
    ));
    let requests = client
        .requests
        .lock()
        .expect("request recorder should lock");
    assert_eq!(requests[0].instructions, "Follow the schema");
    assert_eq!(requests[0].input[0].get("role"), Some(&json!("user")));
    assert_eq!(
        requests[0].reasoning_effort,
        Some(crate::ReasoningEffort::High)
    );
}

#[tokio::test]
async fn durable_session_replays_encrypted_reasoning_with_assistant_output() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let client = Arc::new(FixedClient::default());
    let backend = CodexBackend::with_client("gpt-test", client.clone());
    let model = Codex::with_backend(backend).expect("Codex metadata should be valid");
    let harness = Harness::new(model)
        .database(directory.path().join("harness.db"))
        .model_reasoning_effort(ReasoningEffort::High);
    let mut session = harness
        .session("codex-replay", person_schema())
        .create()
        .await
        .expect("session should be created");

    // Act
    session
        .send("Extract the name")
        .await
        .expect("first turn should complete");
    session
        .send("Repeat the name")
        .await
        .expect("second turn should complete");

    // Assert
    let requests = client
        .requests
        .lock()
        .expect("request recorder should lock");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].reasoning_effort, Some(ReasoningEffort::High));
    assert_eq!(
        requests[1].replay_account_fingerprint.as_deref(),
        Some("account-fingerprint")
    );
    assert_eq!(requests[1].input.len(), 4);
    assert_eq!(requests[1].input[0].get("role"), Some(&json!("user")));
    assert_eq!(requests[1].input[1].get("type"), Some(&json!("reasoning")));
    assert_eq!(requests[1].input[2].get("role"), Some(&json!("assistant")));
    assert_eq!(requests[1].input[3].get("role"), Some(&json!("user")));
}

#[tokio::test]
async fn durable_replay_accepts_equivalent_formatted_json() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let client = Arc::new(FormattedClient::default());
    let model = Codex::with_backend(CodexBackend::with_client("gpt-test", client.clone()))
        .expect("Codex metadata should be valid");
    let harness = Harness::new(model).database(directory.path().join("harness.db"));
    let mut session = harness
        .session("formatted-replay", person_schema())
        .create()
        .await
        .expect("session should be created");

    // Act
    session
        .send("Extract the name")
        .await
        .expect("formatted first turn should complete");
    session
        .send("Repeat the name")
        .await
        .expect("canonicalized replay should complete");

    // Assert
    let requests = client.requests.lock().expect("requests should lock");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].input[1].get("type"), Some(&json!("reasoning")));
}

#[tokio::test]
async fn durable_account_context_survives_history_eviction_and_restart() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let database_path = directory.path().join("harness.db");
    let first_model = Codex::with_backend(CodexBackend::with_client(
        "gpt-test",
        Arc::new(FixedClient::default()),
    ))
    .expect("Codex metadata should be valid");
    let first_harness = Harness::new(first_model)
        .database(&database_path)
        .max_history_bytes(NonZeroUsize::new(1).expect("history limit"));
    let mut first_session = first_harness
        .session("evicted-account", person_schema())
        .create()
        .await
        .expect("session should be created");
    first_session
        .send("Extract the name")
        .await
        .expect("first turn should complete");
    drop(first_session);
    let resumed_client = Arc::new(FixedClient::default());
    let resumed_model = Codex::with_backend(CodexBackend::with_client(
        "gpt-test",
        resumed_client.clone(),
    ))
    .expect("Codex metadata should be valid");
    let resumed_harness = Harness::new(resumed_model).database(database_path);
    let mut resumed = resumed_harness
        .resume("evicted-account")
        .await
        .expect("session should resume");

    // Act
    resumed
        .send("Repeat the name")
        .await
        .expect("resumed turn should complete");

    // Assert
    let requests = resumed_client
        .requests
        .lock()
        .expect("requests should lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].input.len(), 1);
    assert_eq!(
        requests[0].replay_account_fingerprint.as_deref(),
        Some("account-fingerprint")
    );
}

#[tokio::test]
async fn backend_rejects_unsupported_capabilities_without_a_request() {
    // Arrange
    let client = Arc::new(FixedClient::default());
    let backend = CodexBackend::with_client("gpt-test", client.clone());
    let tool_request = ModelRequest::new("Read", person_schema()).with_tool(ToolDefinition::read());
    let array_schema =
        OutputSchema::new(json!({ "type": "array" })).expect("array schema should compile");
    let incompatible_schemas = [
        json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"]
        }),
        json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": [],
            "additionalProperties": false
        }),
        json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false,
            "allOf": [{
                "type": "object",
                "properties": {},
                "required": [],
                "additionalProperties": false
            }]
        }),
    ]
    .map(|schema| OutputSchema::new(schema).expect("schema should compile locally"));

    // Act
    let tool_error = backend.generate(&tool_request).await.err();
    let schema_error = backend
        .generate(&ModelRequest::new("Array", array_schema))
        .await
        .err();
    let mut incompatible_errors = Vec::new();
    for schema in incompatible_schemas {
        incompatible_errors.push(backend.generate(&ModelRequest::new("Object", schema)).await);
    }

    // Assert
    assert_eq!(
        tool_error
            .as_ref()
            .expect("tools should be rejected")
            .error_type(),
        crate::model::ModelErrorType::UnsupportedCapability
    );
    assert!(matches!(
        schema_error,
        Some(ModelError::UnsupportedOutputSchema { .. })
    ));
    assert!(
        incompatible_errors
            .iter()
            .all(|result| matches!(result, Err(ModelError::UnsupportedOutputSchema { .. })))
    );
    assert!(
        client
            .requests
            .lock()
            .expect("request recorder should lock")
            .is_empty()
    );
}

#[tokio::test]
async fn backend_rejects_mismatched_durable_account_context() {
    // Arrange
    let client = Arc::new(FixedClient::default());
    let backend = CodexBackend::with_client("gpt-test", client.clone());
    let replay = json!({
        "account_fingerprint": "account-b",
        "items": [{
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": r#"{"name":"Ada"}"# }]
        }]
    });
    let mut request = ModelRequest::with_history(
        vec![ModelMessage::AssistantReasoning {
            content: r#"{"name":"Ada"}"#.to_string(),
            reasoning_content: replay.to_string(),
        }],
        "Repeat",
        person_schema(),
    );
    request.set_provider_context(Some("account-a".to_string()));

    // Act
    let error = backend.generate(&request).await.err();

    // Assert
    assert!(matches!(error, Some(ModelError::Request { .. })));
    assert!(
        client
            .requests
            .lock()
            .expect("requests should lock")
            .is_empty()
    );
}

#[test]
fn strict_schema_validation_covers_nested_and_malformed_shapes() {
    // Arrange
    let schemas = [
        json!({
            "type": "object",
            "properties": { "value": true },
            "required": ["value"],
            "additionalProperties": false
        }),
        json!({
            "type": ["object", "null"],
            "required": [],
            "additionalProperties": false
        }),
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        json!({
            "type": "object",
            "properties": {
                "values": { "type": "array", "items": true }
            },
            "required": ["values"],
            "additionalProperties": false
        }),
        json!({
            "type": "object",
            "properties": {
                "value": { "anyOf": [{ "type": "string" }, { "type": "null" }] }
            },
            "required": ["value"],
            "additionalProperties": false,
            "$defs": { "entry": { "type": "string" } }
        }),
        json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false,
            "definitions": "invalid"
        }),
    ];

    // Act
    let nullable_object = validate_strict_schema_node(&schemas[1]);
    let results = schemas.map(|schema| validate_strict_output_schema(&schema));

    // Assert
    assert!(results[..4].iter().all(Result::is_err));
    assert!(results[4].is_ok());
    assert!(results[5].is_err());
    assert!(nullable_object.is_err());
}

#[test]
fn prompt_maps_conversation_and_rejects_tool_history() {
    // Arrange
    let messages = [
        ModelMessage::Assistant("prior".to_string()),
        ModelMessage::AssistantReasoning {
            content: "reasoned".to_string(),
            reasoning_content: json!({
                "account_fingerprint": "account-fingerprint",
                "items": [
                    {
                        "type": "reasoning",
                        "id": "reasoning-1",
                        "encrypted_content": "opaque"
                    },
                    {
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": "reasoned" }]
                    }
                ]
            })
            .to_string(),
        },
        ModelMessage::User("next".to_string()),
    ];
    let blank_system_messages = [
        ModelMessage::System("  \n".to_string()),
        ModelMessage::User("next".to_string()),
    ];
    let tool_result = ModelMessage::ToolResult {
        call_id: "call-1".to_string(),
        content: "contents".to_string(),
        name: "read".to_string(),
    };

    // Act
    let prompt = CodexPrompt::from_messages(&messages).expect("messages should map");
    let blank_system_prompt = CodexPrompt::from_messages(&blank_system_messages)
        .expect("blank system prompt should use the default");
    let error = CodexPrompt::from_messages(&[tool_result]).err();
    let invalid_reasoning = [
        "not-json",
        "[]",
        r#"[{"type":"message","role":"assistant"}]"#,
        r#"[{"type":"reasoning"},{"type":"message","role":"user"}]"#,
        concat!(
            r#"[{"type":"reasoning"},{"type":"message","role":"assistant","phase":"#,
            r#"commentary"}]"#
        ),
        r#"[{"type":"reasoning"},{"type":"unknown"}]"#,
    ]
    .map(|reasoning_content| {
        CodexPrompt::from_messages(&[ModelMessage::AssistantReasoning {
            content: "reasoned".to_string(),
            reasoning_content: reasoning_content.to_string(),
        }])
        .err()
        .expect("invalid reasoning should be rejected")
    });

    // Assert
    assert_eq!(prompt.instructions, DEFAULT_INSTRUCTIONS);
    assert_eq!(
        prompt.replay_account_fingerprint.as_deref(),
        Some("account-fingerprint")
    );
    assert_eq!(blank_system_prompt.instructions, DEFAULT_INSTRUCTIONS);
    assert_eq!(prompt.input[0].get("role"), Some(&json!("assistant")));
    assert_eq!(prompt.input[1].get("type"), Some(&json!("reasoning")));
    assert_eq!(prompt.input[2].get("role"), Some(&json!("assistant")));
    assert_eq!(prompt.input[3].get("role"), Some(&json!("user")));
    assert_eq!(
        error
            .as_ref()
            .expect("tool history should be rejected")
            .error_type(),
        crate::model::ModelErrorType::UnsupportedCapability
    );
    assert!(
        invalid_reasoning
            .iter()
            .all(|error| error.error_type() == crate::model::ModelErrorType::Request)
    );
}

#[test]
fn replay_state_requires_a_consistent_account_fingerprint() {
    // Arrange
    let replay = |account_fingerprint: &str| {
        json!({
            "account_fingerprint": account_fingerprint,
            "items": [{
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "reasoned" }]
            }]
        })
        .to_string()
    };
    let messages = [
        ModelMessage::AssistantReasoning {
            content: "reasoned".to_string(),
            reasoning_content: replay("account-a"),
        },
        ModelMessage::AssistantReasoning {
            content: "reasoned".to_string(),
            reasoning_content: replay("account-b"),
        },
    ];

    // Act
    let persisted = persisted_replay(Some("account-a".to_string()), None, "reasoned")
        .expect("account provenance should persist without reasoning items");
    let persisted_prompt = CodexPrompt::from_messages(&[ModelMessage::AssistantReasoning {
        content: "reasoned".to_string(),
        reasoning_content: persisted,
    }])
    .expect("persisted account provenance should replay");
    let missing_fingerprint = persisted_replay(None, None, "reasoned").err();
    let malformed_items = persisted_replay(
        Some("account-a".to_string()),
        Some("not-json".to_string()),
        "reasoned",
    )
    .err();
    let blank_fingerprint = CodexPrompt::from_messages(&[ModelMessage::AssistantReasoning {
        content: "reasoned".to_string(),
        reasoning_content: replay(""),
    }])
    .err();
    let changed_fingerprint = CodexPrompt::from_messages(&messages).err();
    let interleaved = persisted_replay(
        Some("account-a".to_string()),
        Some(
            json!([
                { "type": "reasoning", "encrypted_content": "one" },
                {
                    "type": "message",
                    "role": "assistant",
                    "phase": "commentary",
                    "content": [{ "type": "output_text", "text": "Working" }]
                },
                { "type": "reasoning", "encrypted_content": "two" },
                {
                    "type": "message",
                    "role": "assistant",
                    "phase": "final_answer",
                    "content": [
                        { "type": "output_text", "text": "{\n  \"name\":" },
                        { "type": "output_text", "text": " \"Ada\"\n}" }
                    ]
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "phase": "commentary",
                    "content": [{ "type": "output_text", "text": "Done" }]
                }
            ])
            .to_string(),
        ),
        r#"{"name":"Ada"}"#,
    )
    .expect("commentary replay with equivalent JSON should persist");
    let interleaved_prompt = CodexPrompt::from_messages(&[ModelMessage::AssistantReasoning {
        content: r#"{"name":"Ada"}"#.to_string(),
        reasoning_content: interleaved,
    }])
    .expect("interleaved replay should validate");

    // Assert
    assert_eq!(
        persisted_prompt.replay_account_fingerprint.as_deref(),
        Some("account-a")
    );
    assert_eq!(persisted_prompt.input.len(), 1);
    assert_eq!(interleaved_prompt.input.len(), 5);
    for error in [
        missing_fingerprint,
        malformed_items,
        blank_fingerprint,
        changed_fingerprint,
    ] {
        assert_eq!(
            error
                .expect("invalid replay state should fail")
                .error_type(),
            crate::model::ModelErrorType::Request
        );
    }
}

#[tokio::test]
async fn codex_exposes_public_metadata_and_observer_configuration() {
    // Arrange
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed_events = events.clone();
    let backend = CodexBackend::with_client("gpt-test", Arc::new(FixedClient::default()));
    let model = Codex::with_backend(backend)
        .expect("Codex metadata should be valid")
        .with_lifecycle_observer(move |event| {
            observed_events
                .lock()
                .expect("events should lock")
                .push(event);
        });

    // Act
    let metadata = Model::metadata(&model).expect("metadata should be available");
    let completion = Model::complete(&model, ModelRequest::new("Extract", person_schema()))
        .await
        .expect("Codex request should complete");

    // Assert
    assert_eq!(metadata.provider(), "openai");
    assert_eq!(metadata.model(), "gpt-test");
    assert_eq!(
        completion.response().output(),
        Some(&json!({ "name": "Ada" }))
    );
    assert!(!events.lock().expect("events should lock").is_empty());
}
