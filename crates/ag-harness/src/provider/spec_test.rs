//! Open provider specification tests: each harness option maps onto the
//! routed specification, `ModelClient::from_spec` wires one to the wire, and
//! invalid or modelless specifications fail before any client exists.

use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{
    Auth, CredentialSource, EffortNames, InstructionPlacement, Jitter, MaxTokensField,
    ProviderSpec, ProviderSpecError, ReasoningMapping, RetryKind, RetryPolicy,
    StructuredOutputMode, ThinkingOptions, WireProtocol,
};
use crate::model::{
    Model, ModelClient, ModelMetadata, ModelMetadataError, ModelRequest, ReasoningEffort,
    ReasoningFamily,
};
use crate::schema_contract::OutputSchema;
use crate::telemetry;

const BASE_URL: &str = "https://gateway.example/v1";

/// Credential returning a fixed token.
struct FixedToken;

#[async_trait]
impl CredentialSource for FixedToken {
    async fn bearer_token(&self) -> Result<String, Box<dyn Error + Send + Sync>> {
        Ok("credential-token".to_string())
    }
}

/// Router credential standing in for the harness one in expected specs.
struct RouterToken;

#[async_trait]
impl ag_router::CredentialSource for RouterToken {
    async fn bearer_token(&self) -> Result<String, Box<dyn Error + Send + Sync>> {
        Ok("credential-token".to_string())
    }
}

fn routed(spec: ProviderSpec) -> String {
    format!("{:?}", spec.into_parts().2)
}

fn router_spec(protocol: ag_router::WireProtocol) -> ag_router::ProviderSpec {
    ag_router::ProviderSpec::new("custom", protocol, BASE_URL).with_display_name("gateway")
}

#[test]
fn maps_every_option_onto_the_routed_specification() {
    // Arrange
    let chat = ProviderSpec::new("gateway", WireProtocol::ChatCompletions, BASE_URL)
        .with_auth(Auth::Header {
            name: "api-key".to_string(),
            value: "azure-key".to_string(),
        })
        .with_header("HTTP-Referer", "https://agentty.dev")
        .with_query("api-version", "2026-01-01")
        .with_image_input(true)
        .with_max_tokens_field(Some(MaxTokensField::MaxCompletionTokens))
        .with_prompt_cache_key(true)
        .with_reasoning(ReasoningMapping::Thinking(
            ThinkingOptions::default()
                .with_disable_supported(true)
                .with_preserve_reasoning(true),
        ))
        .with_response_format_with_tools(true)
        .with_structured_output(StructuredOutputMode::JsonObject)
        .with_reasoning_text_replay(true)
        .with_instruction_placement(InstructionPlacement::JoinPreviousUser)
        .with_tool_result_name(true)
        .with_overflow_marker("Prompt is too long");
    let variants = [
        (
            ProviderSpec::new("gateway", WireProtocol::Responses, BASE_URL)
                .with_auth(Auth::Bearer("key".to_string()))
                .with_max_tokens_field(Some(MaxTokensField::MaxOutputTokens))
                .with_reasoning(ReasoningMapping::Effort(
                    EffortNames::default().with(ReasoningEffort::Low, "minimal"),
                ))
                .with_structured_output(StructuredOutputMode::JsonSchema),
            router_spec(ag_router::WireProtocol::Responses)
                .with_auth(ag_router::Auth::Bearer("key".to_string()))
                .with_max_tokens_field(Some(ag_router::MaxTokensField::MaxOutputTokens))
                .with_reasoning(ag_router::ReasoningMapping::Effort(
                    ag_router::EffortNames::default()
                        .with(ag_router::ReasoningEffort::Low, "minimal"),
                ))
                .with_structured_output(ag_router::StructuredOutputMode::JsonSchema),
        ),
        (
            ProviderSpec::new("gateway", WireProtocol::ChatCompletions, BASE_URL)
                .with_auth(Auth::Credential(Arc::new(FixedToken)))
                .with_max_tokens_field(Some(MaxTokensField::MaxTokens))
                .with_reasoning(ReasoningMapping::EnableThinking)
                .with_structured_output(StructuredOutputMode::SchemaPrompt),
            router_spec(ag_router::WireProtocol::ChatCompletions)
                .with_auth(ag_router::Auth::Credential(Arc::new(RouterToken)))
                .with_max_tokens_field(Some(ag_router::MaxTokensField::MaxTokens))
                .with_reasoning(ag_router::ReasoningMapping::EnableThinking)
                .with_structured_output(ag_router::StructuredOutputMode::SchemaPrompt),
        ),
        (
            ProviderSpec::new("gateway", WireProtocol::ChatCompletions, BASE_URL)
                .with_auth(Auth::None)
                .with_instruction_placement(InstructionPlacement::Separate)
                .with_max_tokens_field(None)
                .with_reasoning(ReasoningMapping::None),
            router_spec(ag_router::WireProtocol::ChatCompletions)
                .with_instruction_placement(ag_router::InstructionPlacement::Separate)
                .with_max_tokens_field(None)
                .with_reasoning(ag_router::ReasoningMapping::None),
        ),
    ];
    let expected_chat = router_spec(ag_router::WireProtocol::ChatCompletions)
        .with_auth(ag_router::Auth::Header {
            name: "api-key".to_string(),
            value: "azure-key".to_string(),
        })
        .with_header("HTTP-Referer", "https://agentty.dev")
        .with_query("api-version", "2026-01-01")
        .with_image_input(true)
        .with_max_tokens_field(Some(ag_router::MaxTokensField::MaxCompletionTokens))
        .with_prompt_cache_key(true)
        .with_reasoning(ag_router::ReasoningMapping::Thinking(
            ag_router::ThinkingOptions::default()
                .with_disable_supported(true)
                .with_preserve_reasoning(true),
        ))
        .with_response_format_with_tools(true)
        .with_structured_output(ag_router::StructuredOutputMode::JsonObject)
        .with_reasoning_text_replay(true)
        .with_instruction_placement(ag_router::InstructionPlacement::JoinPreviousUser)
        .with_tool_result_name(true)
        .with_overflow_marker("Prompt is too long");

    // Act / Assert
    assert_eq!(routed(chat), format!("{expected_chat:?}"));
    for (spec, expected) in variants {
        assert_eq!(routed(spec), format!("{expected:?}"));
    }
}

#[test]
fn maps_every_retry_option_onto_the_routed_policy() {
    // Arrange
    let retried = [
        (
            Jitter::None,
            RetryKind::RateLimited,
            ag_router::Jitter::None,
            ag_router::RetryKind::RateLimited,
        ),
        (
            Jitter::Full,
            RetryKind::ServerError,
            ag_router::Jitter::Full,
            ag_router::RetryKind::ServerError,
        ),
        (
            Jitter::Equal,
            RetryKind::Overloaded,
            ag_router::Jitter::Equal,
            ag_router::RetryKind::Overloaded,
        ),
        (
            Jitter::None,
            RetryKind::Connection,
            ag_router::Jitter::None,
            ag_router::RetryKind::Connection,
        ),
    ];
    let retry_variants = retried.map(|(jitter, kind, router_jitter, router_kind)| {
        let retry = RetryPolicy::none()
            .with_base_delay(Duration::from_millis(250))
            .with_jitter(jitter)
            .with_max_attempts(3)
            .with_max_backoff(Duration::from_secs(2))
            .with_max_connection_retries(2)
            .with_max_retry_after(Duration::from_secs(9))
            .with_retried(kind, true);
        let router_retry = ag_router::RetryPolicy::none()
            .with_base_delay(Duration::from_millis(250))
            .with_jitter(router_jitter)
            .with_max_attempts(3)
            .with_max_backoff(Duration::from_secs(2))
            .with_max_connection_retries(2)
            .with_max_retry_after(Duration::from_secs(9))
            .with_retried(router_kind, true);

        (
            ProviderSpec::new("gateway", WireProtocol::ChatCompletions, BASE_URL).with_retry(retry),
            router_spec(ag_router::WireProtocol::ChatCompletions).with_retry(router_retry),
        )
    });

    // Act / Assert
    assert_eq!(
        routed(
            ProviderSpec::new("gateway", WireProtocol::Responses, BASE_URL)
                .with_retry(RetryPolicy::default())
        ),
        format!("{:?}", router_spec(ag_router::WireProtocol::Responses))
    );
    for (spec, expected) in retry_variants {
        assert_eq!(routed(spec), format!("{expected:?}"));
    }
}

#[tokio::test]
async fn from_spec_sends_requests_through_the_specification() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"finish_reason": "stop", "message": {"content": "{\"name\":\"Ada\"}"}}]
        })))
        .mount(&server)
        .await;
    let spec = ProviderSpec::new(
        "gateway",
        WireProtocol::ChatCompletions,
        format!("{}/v1", server.uri()),
    )
    .with_auth(Auth::Credential(Arc::new(FixedToken)))
    .with_header("X-Title", "Agentty")
    .with_query("api-version", "2026-01-01");
    let schema = OutputSchema::new(json!({
        "type": "object",
        "properties": {"name": {"type": "string"}},
        "required": ["name"],
        "additionalProperties": false
    }))
    .expect("valid schema");
    let client =
        ModelClient::from_spec(spec.for_model("gateway-model")).expect("valid specification");

    // Act
    let result = client
        .complete(ModelRequest::new("Find the name", schema))
        .await;

    // Assert
    assert!(result.is_ok(), "{result:?}");
    let requests = server.received_requests().await.expect("recording");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url.query(), Some("api-version=2026-01-01"));
    assert_eq!(
        requests[0].headers.get("authorization").expect("auth"),
        "Bearer credential-token"
    );
    assert_eq!(
        requests[0].headers.get("x-title").expect("title"),
        "Agentty"
    );
    assert_eq!(client.metadata().provider(), "gateway");
    assert_eq!(client.metadata().model(), "gateway-model");
}

#[test]
fn from_spec_rejects_invalid_unnamed_and_modelless_specifications() {
    // Arrange
    let blank_key = ProviderSpec::new("gateway", WireProtocol::ChatCompletions, BASE_URL)
        .with_auth(Auth::Bearer(" ".to_string()))
        .for_model("gateway-model");
    let unnamed = ProviderSpec::new(" ", WireProtocol::Responses, BASE_URL).for_model("model");
    let modelless = ProviderSpec::new("gateway", WireProtocol::ChatCompletions, BASE_URL);
    let blank_model = modelless.clone().for_model(" ");

    // Act
    let invalid = ModelClient::from_spec(blank_key.clone());
    let unnamed = ModelClient::from_spec(unnamed);
    let missing = ModelClient::from_spec(modelless.clone());
    let blank = ModelClient::from_spec(blank_model);
    let served = ModelClient::from_spec(modelless.clone().for_model("kimi-k2.6"));

    // Assert
    assert!(matches!(
        &invalid,
        Err(ProviderSpecError::Invalid { reason })
            if reason == "provider specification is invalid: bearer key is blank"
    ));
    assert!(matches!(
        blank_key.validate(),
        Err(ProviderSpecError::Invalid { .. })
    ));
    assert!(matches!(
        unnamed,
        Err(ProviderSpecError::Metadata(
            ModelMetadataError::EmptyProvider
        ))
    ));
    assert_eq!(
        missing.err().map(|error| error.to_string()).as_deref(),
        Some("provider specification names no model")
    );
    assert!(matches!(
        blank,
        Err(ProviderSpecError::Metadata(ModelMetadataError::EmptyModel))
    ));
    assert_eq!(
        served.expect("a named model is served").metadata().model(),
        "kimi-k2.6"
    );
    assert_eq!(modelless.model(), None);
}

#[test]
fn new_derives_the_reasoning_family_from_the_provider_name() {
    // Arrange
    let names = [
        telemetry::PROVIDER_OPENAI,
        telemetry::PROVIDER_OPENAI_CHATGPT,
        telemetry::PROVIDER_OPENAI_COMPATIBLE,
        telemetry::PROVIDER_MOONSHOT_AI,
        "gateway",
    ];

    // Act
    let families = names.map(|name| {
        (
            ProviderSpec::new(name, WireProtocol::ChatCompletions, BASE_URL)
                .reasoning_family()
                .clone(),
            ModelMetadata::new(name, "model")
                .expect("valid metadata")
                .reasoning_family()
                .clone(),
        )
    });

    // Assert
    for (family, metadata_family) in &families {
        assert_eq!(family, metadata_family);
    }
    assert_eq!(
        families[0].0,
        ReasoningFamily::Providers(vec!["openai_chatgpt".into()])
    );
    assert_eq!(families[2].0, ReasoningFamily::Model);
    assert_eq!(families[4].0, ReasoningFamily::Provider);
}

#[test]
fn debug_output_never_prints_secrets() {
    // Arrange
    let auths = [
        (Auth::None, "None"),
        (Auth::Bearer("secret".to_string()), "Bearer(<redacted>)"),
        (
            Auth::Header {
                name: "api-key".to_string(),
                value: "secret".to_string(),
            },
            "Header(api-key: <redacted>)",
        ),
        (Auth::Credential(Arc::new(FixedToken)), "Credential"),
    ];
    let spec = ProviderSpec::new("gateway", WireProtocol::ChatCompletions, BASE_URL)
        .with_auth(Auth::Bearer("secret".to_string()))
        .with_header("x-gateway-key", "secret")
        .with_query("key", "secret");

    // Act
    let debug = format!("{spec:?}");

    // Assert
    for (auth, expected) in auths {
        assert_eq!(format!("{auth:?}"), expected);
    }
    assert!(debug.contains("gateway"), "{debug}");
    assert!(!debug.contains("secret"), "{debug}");
}

#[test]
fn names_the_provider_for_telemetry() {
    // Arrange / Act
    let spec = ProviderSpec::new(
        String::from("gateway"),
        WireProtocol::ChatCompletions,
        BASE_URL,
    );

    // Assert
    assert_eq!(spec.name(), "gateway");
}

#[test]
fn from_spec_carries_the_reasoning_family_and_text_replay() {
    // Arrange
    let plain = ProviderSpec::new("gateway", WireProtocol::ChatCompletions, BASE_URL);
    let shared = ProviderSpec::new("gateway", WireProtocol::ChatCompletions, BASE_URL)
        .with_reasoning_family(ReasoningFamily::Model)
        .with_reasoning_text_replay(true);

    // Act
    let plain_client = ModelClient::from_spec(plain.clone().for_model("gateway-model"));
    let shared_client = ModelClient::from_spec(shared.clone().for_model("gateway-model"));

    // Assert
    assert_eq!(plain.reasoning_family(), &ReasoningFamily::Provider);
    assert_eq!(shared.reasoning_family(), &ReasoningFamily::Model);
    let plain_client = plain_client.expect("valid specification");
    let shared_client = shared_client.expect("valid specification");
    assert_eq!(
        plain_client.metadata().reasoning_family(),
        &ReasoningFamily::Provider
    );
    assert_eq!(
        shared_client.metadata().reasoning_family(),
        &ReasoningFamily::Model
    );
    assert!(!Model::replays_reasoning_text(&plain_client));
    assert!(Model::replays_reasoning_text(&shared_client));
}

#[cfg(feature = "providers-openai")]
#[test]
fn openai_presets_share_one_reasoning_family() {
    // Arrange
    let openai = ProviderSpec::openai(super::super::OpenAiConfig {
        api_key: "key".to_string(),
        base_url: BASE_URL.to_string(),
        model: "gpt-6-luna".to_string(),
    });
    let chatgpt = ProviderSpec::chatgpt(super::super::ChatGptConfig {
        auth_file: "auth.json".into(),
        base_url: BASE_URL.to_string(),
        model: "gpt-6-luna".to_string(),
    });

    // Act
    let families = [openai.reasoning_family(), chatgpt.reasoning_family()];

    // Assert
    assert_eq!(
        families,
        [
            &ReasoningFamily::for_provider(openai.name()),
            &ReasoningFamily::for_provider(chatgpt.name())
        ]
    );
    assert_eq!(
        families[0],
        &ReasoningFamily::Providers(vec!["openai_chatgpt".into()])
    );
    assert_eq!(
        families[1],
        &ReasoningFamily::Providers(vec!["openai".into()])
    );
}

#[cfg(feature = "providers-compat")]
#[test]
fn compatible_presets_keep_their_reasoning_families() {
    // Arrange
    let kimi = ProviderSpec::kimi(super::super::KimiConfig {
        api_key: "key".to_string(),
        base_url: BASE_URL.to_string(),
        model: "kimi-k3".to_string(),
    });
    let muse = ProviderSpec::muse(super::super::MuseConfig {
        api_key: "key".to_string(),
        base_url: BASE_URL.to_string(),
        model: "muse-spark-1.3".to_string(),
    });
    let qwen = ProviderSpec::qwen(super::super::QwenConfig {
        api_key: "key".to_string(),
        base_url: BASE_URL.to_string(),
        model: "qwen3.8-max".to_string(),
    });
    let compatible = ProviderSpec::openai_compatible(super::super::OpenAiCompatibleConfig {
        api_key: None,
        base_url: BASE_URL.to_string(),
        model: "llama3.3".to_string(),
    });

    // Act
    let families = [&kimi, &muse, &qwen, &compatible].map(|spec| {
        (
            spec.reasoning_family().clone(),
            ReasoningFamily::for_provider(spec.name()),
        )
    });

    // Assert
    for (family, expected) in &families {
        assert_eq!(family, expected);
    }
    assert_eq!(families[0].0, ReasoningFamily::Provider);
    assert_eq!(families[3].0, ReasoningFamily::Model);
}

#[cfg(feature = "providers-openai")]
#[tokio::test]
async fn chatgpt_sign_in_is_a_harness_credential_source() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory");
    let auth: Arc<dyn CredentialSource> = Arc::new(super::super::ChatGptAuth::new(
        directory.path().join("auth.json"),
    ));

    // Act
    let token = auth.bearer_token().await;
    let spec = ProviderSpec::new("plan", WireProtocol::Responses, BASE_URL)
        .with_auth(Auth::Credential(auth));

    // Assert
    assert!(token.is_err(), "no sign-in is stored");
    assert!(spec.validate().is_ok());
}
