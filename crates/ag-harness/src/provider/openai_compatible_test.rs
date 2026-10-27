use std::env;

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{
    DEFAULT_CONTEXT_WINDOW_TOKENS, DEFAULT_MAX_OUTPUT_TOKENS, OPENAI_COMPATIBLE_CONTEXT_WINDOW_ENV,
    OPENAI_COMPATIBLE_MAX_OUTPUT_TOKENS_ENV, OpenAiCompatibleConfig, profile_from_environment,
};
use crate::model::{HeuristicContextEstimator, Model, ModelClient, ModelProfile, ModelRequest};
use crate::provider::catalog::ModelConfigurationError;
use crate::schema_contract::OutputSchema;
use crate::store_fixture::image_input;
use crate::tool;

fn environment(
    context_window: Option<&'static str>,
    max_output_tokens: Option<&'static str>,
) -> impl FnMut(&str) -> Result<String, env::VarError> {
    move |name| {
        let value = match name {
            OPENAI_COMPATIBLE_CONTEXT_WINDOW_ENV => context_window,
            OPENAI_COMPATIBLE_MAX_OUTPUT_TOKENS_ENV => max_output_tokens,
            _ => None,
        };

        value.map(str::to_string).ok_or(env::VarError::NotPresent)
    }
}

fn person_schema_value() -> Value {
    json!({
        "type": "object",
        "properties": {"name": {"type": "string"}},
        "required": ["name"],
        "additionalProperties": false
    })
}

fn client(server: &MockServer) -> ModelClient {
    ModelClient::openai_compatible(OpenAiCompatibleConfig {
        api_key: None,
        base_url: format!("{}/v1", server.uri()),
        model: "qwen3:latest".to_string(),
    })
    .expect("fixture configuration should be valid")
}

#[test]
fn profile_defaults_to_a_conservative_ollama_window() {
    // Arrange and Act
    let profile = profile_from_environment(environment(None, Some(" ")))
        .expect("defaults should form a valid profile");

    // Assert
    assert_eq!(
        profile,
        ModelProfile::new(DEFAULT_CONTEXT_WINDOW_TOKENS)
            .with_estimate_margin(DEFAULT_MAX_OUTPUT_TOKENS, 50)
            .with_estimator(HeuristicContextEstimator::DEFAULT.with_output_schema_in_prompt())
    );
    assert!(profile.context_budget().is_ok());
}

#[test]
fn profile_uses_the_declared_window_and_output_reservation() {
    // Arrange and Act
    let profile = profile_from_environment(environment(Some(" 40960 "), Some("4096")))
        .expect("declared counts should form a valid profile");

    // Assert
    assert_eq!(profile.context_window_tokens(), 40_960);
    assert_eq!(
        profile,
        ModelProfile::new(40_960)
            .with_estimate_margin(4_096, 50)
            .with_estimator(HeuristicContextEstimator::DEFAULT.with_output_schema_in_prompt())
    );
}

#[test]
fn profile_rejects_invalid_or_oversized_token_counts() {
    // Arrange
    let cases = [
        (
            Some("lots"),
            None,
            OPENAI_COMPATIBLE_CONTEXT_WINDOW_ENV,
            "lots",
        ),
        (
            None,
            Some("0"),
            OPENAI_COMPATIBLE_MAX_OUTPUT_TOKENS_ENV,
            "0",
        ),
    ];

    for (context_window, max_output_tokens, expected_name, expected_value) in cases {
        // Act
        let error = profile_from_environment(environment(context_window, max_output_tokens))
            .expect_err("invalid count should fail");

        // Assert
        assert!(
            matches!(&error, ModelConfigurationError::InvalidTokenCount { name, value }
                if *name == expected_name && value == expected_value),
            "{error}"
        );
    }
    let oversized = profile_from_environment(environment(Some("8192"), Some("8192")))
        .expect_err("a reservation filling the window should fail");
    assert_eq!(
        oversized.to_string(),
        "the 8192-token output reservation must be smaller than the 8192-token context window"
    );
}

#[test]
fn profile_rejects_a_non_unicode_count() {
    // Arrange and Act
    let error = profile_from_environment(|_| Err(env::VarError::NotUnicode("invalid".into())))
        .expect_err("non-Unicode count should fail");

    // Assert
    assert!(matches!(
        error,
        ModelConfigurationError::Environment {
            name: OPENAI_COMPATIBLE_CONTEXT_WINDOW_ENV,
            source: env::VarError::NotUnicode(_)
        }
    ));
}

#[test]
fn client_states_the_schema_in_the_prompt_and_rejects_images() {
    // Arrange
    let schema = OutputSchema::new(person_schema_value()).expect("schema should be valid");
    let model = ModelClient::openai_compatible(OpenAiCompatibleConfig {
        api_key: None,
        base_url: "http://127.0.0.1:11434/v1".to_string(),
        model: "qwen3:latest".to_string(),
    })
    .expect("fixture configuration should be valid");

    // Act
    let image = Model::validate_input(&model, &image_input("look", b"payload", "closely"));

    // Assert
    assert_eq!(model.metadata().provider(), "openai_compatible");
    assert_eq!(model.metadata().model(), "qwen3:latest");
    assert!(Model::keeps_prefix_across_schemas(&model));
    assert!(Model::validate_schema(&model, &schema).is_ok());
    assert!(image.is_err());
}

#[tokio::test]
async fn unauthenticated_read_request_decodes_the_tool_call() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": "",
                    "reasoning": "I should read the manifest.",
                    "tool_calls": [{
                        "id": "call_local_read",
                        "index": 0,
                        "type": "function",
                        "function": {"name": "read", "arguments": r#"{"path":"Cargo.toml"}"#}
                    }]
                }
            }],
            "usage": {"prompt_tokens": 900, "completion_tokens": 40, "total_tokens": 940}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let schema = OutputSchema::new(person_schema_value()).expect("schema should be valid");
    let request = ModelRequest::new("inspect the manifest", schema)
        .with_tool(tool::ToolDefinition::read())
        .with_model_reasoning_effort(crate::model::ReasoningEffort::High);

    // Act
    let response = client(&server)
        .complete(request)
        .await
        .expect("local read request should decode");
    let requests = server
        .received_requests()
        .await
        .expect("request recording should be enabled");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("body should be JSON");

    // Assert
    let call = response
        .call()
        .expect("response should contain a tool call");
    assert_eq!(call.id(), "call_local_read");
    assert_eq!(
        call.read_arguments()
            .map(|arguments| arguments.path().to_string()),
        Some("Cargo.toml".to_string())
    );
    assert!(!requests[0].headers.contains_key("authorization"));
    assert_eq!(body["model"], "qwen3:latest");
    assert_eq!(body["tools"][0]["function"]["name"], "read");
    assert!(body.get("reasoning_effort").is_none());
    assert!(body.get("response_format").is_none());
    assert_eq!(
        body["messages"][0]["content"],
        format!(
            "Call the offered tools whenever you need them. When you give your final answer, \
             return only one JSON object that validates against this JSON Schema, with no \
             Markdown fences or other text.\n\nJSON Schema:\n{}",
            person_schema_value()
        )
    );
    assert_eq!(
        response
            .metadata()
            .and_then(|metadata| metadata.usage())
            .and_then(|usage| usage.input_tokens()),
        Some(900)
    );
}
