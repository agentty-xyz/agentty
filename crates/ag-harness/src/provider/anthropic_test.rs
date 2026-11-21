//! Anthropic provider tests for model context profiles, output limits, and
//! client capability and request policy.

use std::num::NonZeroU32;

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{
    AnthropicConfig, CLAUDE_FABLE_5_2, CLAUDE_HAIKU_5_5, CLAUDE_OPUS_5_5, CLAUDE_SONNET_5_5,
    max_output_tokens, profile,
};
use crate::model::{HeuristicContextEstimator, Model, ModelClient, ModelRequest};
use crate::request_option::GenerationParams;
use crate::schema_contract::OutputSchema;
use crate::store_fixture::image_input;

/// Every cataloged Claude model.
const CATALOGED: [&str; 4] = [
    CLAUDE_FABLE_5_2,
    CLAUDE_OPUS_5_5,
    CLAUDE_SONNET_5_5,
    CLAUDE_HAIKU_5_5,
];

/// Returns a client for `model` served from `base_url`.
fn client(model: &str, base_url: &str) -> ModelClient {
    ModelClient::anthropic(AnthropicConfig {
        api_key: "claude-key".to_string(),
        base_url: base_url.to_string(),
        model: model.to_string(),
    })
    .expect("fixture configuration should be valid")
}

/// Starts a Messages server answering every request with one structured
/// output.
async fn messages_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_1",
            "model": CLAUDE_OPUS_5_5,
            "content": [{"type": "text", "text": "{\"name\":\"Ada\"}"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        })))
        .mount(&server)
        .await;

    server
}

/// Returns the `max_tokens` of each request `server` received, in order.
async fn sent_max_tokens(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .expect("recording")
        .iter()
        .map(|request| {
            let body: Value = request.body_json().expect("JSON request body");

            body["max_tokens"].clone()
        })
        .collect()
}

fn name_schema() -> OutputSchema {
    OutputSchema::new(json!({
        "type": "object",
        "properties": {"name": {"type": "string"}},
        "required": ["name"],
        "additionalProperties": false
    }))
    .expect("schema should be valid")
}

#[test]
fn profiles_keep_requests_inside_the_documented_window() {
    // Arrange / Act
    let unlisted = profile("claude-unlisted");

    // Assert
    for model in CATALOGED {
        let profile = profile(model).expect("Claude 5 models are cataloged");
        assert_eq!(profile.context_window_tokens(), 1_000_000, "{model}");
        assert_eq!(profile.input_token_budget(), 611_929, "{model}");
        assert_ne!(
            profile.estimator(),
            HeuristicContextEstimator::DEFAULT,
            "{model}"
        );
        assert_eq!(max_output_tokens(model), Some(128_000), "{model}");
    }
    assert_eq!(unlisted, None);
    assert_eq!(max_output_tokens("claude-unlisted"), None);
}

#[test]
fn client_declares_the_cataloged_profile_and_accepts_a_blank_key() {
    // Arrange
    let object = OutputSchema::new(json!({"type": "object"})).expect("schema should be valid");

    // Act
    let cataloged = client(CLAUDE_SONNET_5_5, "https://api.anthropic.com/v1");
    let unlisted = client("claude-unlisted", "https://api.anthropic.com/v1");
    let resume_only = ModelClient::anthropic(AnthropicConfig {
        api_key: String::new(),
        base_url: "http://127.0.0.1".to_string(),
        model: CLAUDE_HAIKU_5_5.to_string(),
    });
    let blank_model = ModelClient::anthropic(AnthropicConfig {
        api_key: "claude-key".to_string(),
        base_url: "https://api.anthropic.com/v1".to_string(),
        model: " ".to_string(),
    });

    // Assert
    assert_eq!(cataloged.metadata().provider(), "anthropic");
    assert_eq!(cataloged.metadata().model(), CLAUDE_SONNET_5_5);
    assert_eq!(Model::profile(&cataloged), profile(CLAUDE_SONNET_5_5));
    assert_eq!(Model::profile(&unlisted), None);
    assert!(Model::replays_turn_instructions(&cataloged));
    assert!(!Model::keeps_prefix_across_schemas(&cataloged));
    assert!(Model::validate_schema(&cataloged, &object).is_ok());
    assert!(Model::validate_input(&cataloged, &image_input("look", b"payload", "closely")).is_ok());
    assert!(resume_only.is_ok(), "a blank key builds a client");
    assert!(blank_model.is_err(), "a blank model is rejected");
}

#[tokio::test]
async fn unbounded_requests_send_the_cataloged_output_limit() {
    // Arrange
    let server = messages_server().await;
    let base_url = format!("{}/v1", server.uri());
    let cataloged = client(CLAUDE_OPUS_5_5, &base_url);
    let unlisted = client("claude-unlisted", &base_url);
    let bounded = ModelRequest::new("Find the name", name_schema()).with_generation(
        GenerationParams::default()
            .with_max_output_tokens(NonZeroU32::new(4_096).expect("nonzero")),
    );

    // Act
    for (client, request) in [
        (
            &cataloged,
            ModelRequest::new("Find the name", name_schema()),
        ),
        (&unlisted, ModelRequest::new("Find the name", name_schema())),
        (&cataloged, bounded),
    ] {
        client
            .complete(request)
            .await
            .expect("the request should succeed");
    }

    // Assert
    assert_eq!(
        sent_max_tokens(&server).await,
        [json!(128_000), json!(16_000), json!(4_096)]
    );
}
