//! Provider specification tests: defaults, endpoints, headers, validation,
//! and secret-free debug output.

use std::error::Error;
use std::num::NonZeroU32;
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::header::AUTHORIZATION;
use serde_json::json;

use crate::credential::CredentialSource;
use crate::input::{ImageContent, ImageMediaType, InputBlock, TurnInput};
use crate::model::{GenerationOptions, ModelError, ReasoningEffort};
use crate::provider_spec::{
    Auth, EffortNames, InstructionPlacement, MaxTokensField, ProviderSpec, ReasoningMapping,
    StreamUsage, StructuredOutputMode, ThinkingOptions, WireProtocol,
};
use crate::schema::OutputSchema;

const BASE_URL: &str = "https://example.com/v1/";

struct StubCredential;

#[async_trait]
impl CredentialSource for StubCredential {
    async fn bearer_token(&self) -> Result<String, Box<dyn Error + Send + Sync>> {
        Ok("stub-token".to_string())
    }
}

fn chat() -> ProviderSpec {
    ProviderSpec::new("custom", WireProtocol::ChatCompletions, BASE_URL)
}

fn responses() -> ProviderSpec {
    ProviderSpec::new("custom", WireProtocol::Responses, BASE_URL)
}

fn anthropic() -> ProviderSpec {
    ProviderSpec::new("custom", WireProtocol::AnthropicMessages, BASE_URL)
}

fn image_input() -> TurnInput {
    let image = ImageContent::new(
        ImageMediaType::Png,
        vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
    )
    .expect("valid image");

    TurnInput::new(vec![InputBlock::Image(image)]).expect("valid input")
}

#[test]
fn starts_from_a_conservative_policy_per_protocol() {
    // Arrange / Act
    let chat = chat();
    let responses = responses();

    // Assert
    assert_eq!(chat.id(), "custom");
    assert_eq!(chat.display_name, "custom");
    assert!(matches!(chat.auth, Auth::None));
    assert_eq!(chat.structured_output, StructuredOutputMode::SchemaPrompt);
    assert_eq!(chat.reasoning, ReasoningMapping::None);
    assert_eq!(
        chat.max_tokens_field,
        Some(MaxTokensField::MaxCompletionTokens)
    );
    assert!(!chat.image_input && !chat.prompt_cache_key);
    assert_eq!(chat.instruction_placement, InstructionPlacement::Separate);
    assert!(!chat.replay.reasoning_text && !chat.replay.tool_result_name);
    assert_eq!(chat.stream_usage, StreamUsage::Requested);
    assert_eq!(responses.stream_usage, StreamUsage::Unrequested);
    assert!(chat.keeps_prefix_across_schemas());
    assert_eq!(
        responses.structured_output,
        StructuredOutputMode::JsonSchema
    );
    assert_eq!(
        responses.reasoning,
        ReasoningMapping::Effort(EffortNames::default())
    );
    assert_eq!(
        responses.max_tokens_field,
        Some(MaxTokensField::MaxOutputTokens)
    );
    assert!(responses.image_input && responses.prompt_cache_key);
    assert!(!responses.keeps_prefix_across_schemas());
    assert!(chat.validate().is_ok() && responses.validate().is_ok());
}

#[test]
fn starts_anthropic_messages_with_native_output_and_a_replaceable_version() {
    // Arrange
    let bound = NonZeroU32::new(4_096).expect("nonzero");

    // Act
    let anthropic = anthropic();
    let bounded = anthropic
        .clone()
        .with_fallback_max_tokens(bound)
        .with_header("Anthropic-Version", "2026-10-21");
    let headers = bounded.request_headers(None).expect("valid headers");

    // Assert
    assert_eq!(
        anthropic.structured_output,
        StructuredOutputMode::JsonSchema
    );
    assert_eq!(
        anthropic.reasoning,
        ReasoningMapping::Effort(EffortNames::default())
    );
    assert_eq!(anthropic.max_tokens_field, Some(MaxTokensField::MaxTokens));
    assert_eq!(anthropic.fallback_max_tokens, None);
    assert_eq!(
        anthropic.headers,
        [("anthropic-version".to_string(), "2023-06-01".to_string())]
    );
    assert!(anthropic.image_input && !anthropic.prompt_cache_key);
    assert!(!anthropic.keeps_prefix_across_schemas());
    assert!(anthropic.validate().is_ok());
    assert_eq!(bounded.fallback_max_tokens, Some(bound));
    assert!(format!("{bounded:?}").contains("fallback_max_tokens: Some(4096)"));
    assert_eq!(
        headers
            .get_all("anthropic-version")
            .iter()
            .collect::<Vec<_>>(),
        ["2026-10-21"]
    );
}

#[test]
fn effort_names_default_to_the_effort_names_and_rename_one_effort() {
    // Arrange
    let names = EffortNames::default();
    let renamed = EffortNames::default().with(ReasoningEffort::Low, "minimal");
    let efforts = [
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
        ReasoningEffort::XHigh,
        ReasoningEffort::Max,
    ];

    // Act / Assert
    for effort in efforts {
        assert_eq!(names.name(effort), effort.as_str());
    }
    assert_eq!(renamed.name(ReasoningEffort::Low), "minimal");
    assert_eq!(renamed.name(ReasoningEffort::Medium), "medium");
}

#[test]
fn appends_query_parameters_to_the_endpoint() {
    // Arrange
    let plain = chat();
    let queried = chat()
        .with_query("api-version", "2026-01-01")
        .with_query("note", "a b&c");

    // Act / Assert
    assert_eq!(
        plain.endpoint("chat/completions"),
        "https://example.com/v1/chat/completions"
    );
    assert_eq!(
        queried.endpoint("chat/completions"),
        "https://example.com/v1/chat/completions?api-version=2026-01-01&note=a+b%26c"
    );
}

#[test]
fn builds_authentication_and_extra_headers() {
    // Arrange
    let bearer = chat()
        .with_auth(Auth::Bearer("key".to_string()))
        .with_header("X-Title", "Agentty");
    let header = chat().with_auth(Auth::Header {
        name: "api-key".to_string(),
        value: "azure".to_string(),
    });
    let credential = chat().with_auth(Auth::Credential(Arc::new(StubCredential)));

    // Act
    let bearer_headers = bearer.request_headers(None).expect("valid headers");
    let header_headers = header.request_headers(None).expect("valid headers");
    let credential_headers = credential
        .request_headers(Some("token"))
        .expect("valid headers");
    let unauthenticated = credential.request_headers(None).expect("valid headers");
    let invalid_token = credential.request_headers(Some("bad\ntoken"));

    // Assert
    let authorization = bearer_headers.get(AUTHORIZATION).expect("authorization");
    assert_eq!(authorization, "Bearer key");
    assert!(authorization.is_sensitive());
    let title = bearer_headers.get("x-title").expect("title");
    assert_eq!(title, "Agentty");
    assert!(title.is_sensitive());
    assert_eq!(bearer_headers.get_all(AUTHORIZATION).iter().count(), 1);
    assert_eq!(header_headers.get("api-key").expect("api-key"), "azure");
    assert!(header_headers.get(AUTHORIZATION).is_none());
    assert_eq!(
        credential_headers
            .get(AUTHORIZATION)
            .expect("authorization"),
        "Bearer token"
    );
    assert!(unauthenticated.is_empty());
    assert!(matches!(
        invalid_token,
        Err(ModelError::InvalidProviderConfig { reason })
            if reason == "authentication token cannot be sent as a header"
    ));
}

#[test]
fn rejects_specifications_no_request_can_be_sent_with() {
    // Arrange
    let base_url = |url: &str| ProviderSpec::new("custom", WireProtocol::ChatCompletions, url);
    let api_key = |name: &str, value: &str| Auth::Header {
        name: name.to_string(),
        value: value.to_string(),
    };
    let invalid = [
        (
            chat().with_auth(Auth::Bearer(" ".to_string())),
            "bearer key is blank",
        ),
        (
            chat().with_auth(api_key("api-key", " ")),
            "authentication header value is blank",
        ),
        (
            chat().with_auth(api_key("bad name", "value")),
            "authentication header `bad name` is invalid",
        ),
        (
            chat().with_header("x-ok", "bad\nvalue"),
            "header `x-ok` is invalid",
        ),
        (
            chat().with_header("Authorization", "Bearer other"),
            "header `Authorization` is set by the router",
        ),
        (
            chat().with_header("Content-Type", "text/plain"),
            "header `Content-Type` is set by the router",
        ),
        (
            chat()
                .with_auth(api_key("api-key", "azure"))
                .with_header("API-Key", "other"),
            "header `API-Key` is set by the router",
        ),
        (
            chat().with_overflow_marker(" "),
            "overflow markers must not be blank",
        ),
        (
            chat().with_max_tokens_field(Some(MaxTokensField::MaxOutputTokens)),
            "the ChatCompletions protocol cannot express the `max_output_tokens` field",
        ),
        (
            responses().with_structured_output(StructuredOutputMode::JsonObject),
            "the Responses protocol cannot express a structured-output mode",
        ),
        (
            responses().with_reasoning(ReasoningMapping::Thinking(ThinkingOptions::default())),
            "the Responses protocol cannot express a reasoning mapping",
        ),
        (
            responses().with_max_tokens_field(Some(MaxTokensField::MaxCompletionTokens)),
            "the Responses protocol cannot express an output-token field",
        ),
        (
            responses().with_reasoning_text_replay(true),
            "the Responses protocol cannot express reasoning-content replay",
        ),
        (
            responses().with_tool_result_name(true),
            "the Responses protocol cannot express reasoning-content replay",
        ),
        (
            responses().with_stream_usage(true),
            "the Responses protocol cannot express `stream_options.include_usage`",
        ),
        (
            chat().with_structured_output(StructuredOutputMode::ForcedTool),
            "the ChatCompletions protocol cannot express forced-tool structured output",
        ),
        (
            responses().with_fallback_max_tokens(NonZeroU32::MIN),
            "the Responses protocol cannot express a fallback `max_tokens`",
        ),
    ];
    let valid = [
        chat()
            .with_auth(Auth::Credential(Arc::new(StubCredential)))
            .with_max_tokens_field(None)
            .with_reasoning(ReasoningMapping::EnableThinking)
            .with_header("x-title", "Agentty"),
        chat().with_auth(api_key("api-key", "azure")),
        base_url("http://127.0.0.1:11434/v1"),
        responses()
            .with_reasoning(ReasoningMapping::None)
            .with_max_tokens_field(None),
    ];

    // Act / Assert
    assert_rejects(invalid);
    for spec in valid {
        assert!(spec.validate().is_ok(), "{spec:?}");
    }
}

#[test]
fn rejects_anthropic_messages_options_it_cannot_express() {
    // Arrange
    let invalid = [
        (
            anthropic().with_structured_output(StructuredOutputMode::JsonObject),
            "the AnthropicMessages protocol cannot express a structured-output mode",
        ),
        (
            anthropic().with_reasoning(ReasoningMapping::EnableThinking),
            "the AnthropicMessages protocol cannot express a reasoning mapping",
        ),
        (
            anthropic().with_max_tokens_field(None),
            "the AnthropicMessages protocol cannot express an output-token field",
        ),
        (
            anthropic().with_instruction_placement(InstructionPlacement::JoinPreviousUser),
            "the AnthropicMessages protocol cannot express instructions placed",
        ),
        (
            anthropic().with_tool_result_name(true),
            "the AnthropicMessages protocol cannot express reasoning-content replay",
        ),
        (
            anthropic().with_reasoning_text_replay(true),
            "the AnthropicMessages protocol cannot express reasoning-content replay",
        ),
        (
            anthropic().with_prompt_cache_key(true),
            "the AnthropicMessages protocol cannot express a prompt cache key",
        ),
        (
            anthropic().with_stream_usage(true),
            "the AnthropicMessages protocol cannot express `stream_options.include_usage`",
        ),
    ];
    let forced_tool = anthropic()
        .with_structured_output(StructuredOutputMode::ForcedTool)
        .with_reasoning(ReasoningMapping::None);

    // Act / Assert
    assert_rejects(invalid);
    assert!(forced_tool.validate().is_ok());
}

#[test]
fn rejects_unroutable_ids_url_credentials_router_owned_auth_and_blank_efforts() {
    // Arrange
    let base_url = |url: &str| ProviderSpec::new("custom", WireProtocol::ChatCompletions, url);
    let base_url_error =
        "base URL must be an absolute http(s) URL without credentials, query, or fragment";
    let header_auth = |name: &str| Auth::Header {
        name: name.to_string(),
        value: "value".to_string(),
    };
    let invalid = [
        (
            ProviderSpec::new(" ", WireProtocol::ChatCompletions, BASE_URL),
            "provider identifier must be nonblank without `/`",
        ),
        (
            ProviderSpec::new("openrouter/meta", WireProtocol::ChatCompletions, BASE_URL),
            "provider identifier must be nonblank without `/`",
        ),
        (base_url(" "), base_url_error),
        (base_url("api.example.com/v1"), base_url_error),
        (base_url("ftp://example.com/v1"), base_url_error),
        (base_url("https://example.com/v1?x=y"), base_url_error),
        (base_url("https://example.com/v1#top"), base_url_error),
        (base_url("https://user@example.com/v1"), base_url_error),
        (base_url("https://:token@example.com/v1"), base_url_error),
        (
            chat().with_auth(header_auth("Host")),
            "authentication header `Host` is set by the router",
        ),
        (
            chat().with_auth(header_auth("Content-Length")),
            "authentication header `Content-Length` is set by the router",
        ),
        (
            chat().with_reasoning(ReasoningMapping::Effort(
                EffortNames::default().with(ReasoningEffort::High, " "),
            )),
            "reasoning effort names must not be blank",
        ),
    ];

    // Act / Assert
    assert_rejects(invalid);
    assert!(
        chat()
            .with_auth(header_auth("Authorization"))
            .validate()
            .is_ok()
    );
}

/// Asserts that each specification fails validation with a reason starting
/// with its expected text.
fn assert_rejects<'expected>(cases: impl IntoIterator<Item = (ProviderSpec, &'expected str)>) {
    for (spec, expected) in cases {
        let result = spec.validate();
        assert!(
            matches!(
                &result,
                Err(ModelError::InvalidProviderConfig { reason }) if reason.starts_with(expected)
            ),
            "{spec:?}: {result:?}"
        );
    }
}

#[test]
fn joined_instructions_are_a_chat_completions_opt_in() {
    // Arrange
    let chat = chat().with_instruction_placement(InstructionPlacement::JoinPreviousUser);
    let separate = responses().with_instruction_placement(InstructionPlacement::Separate);
    let joined = responses().with_instruction_placement(InstructionPlacement::JoinPreviousUser);

    // Act
    let result = joined.validate();

    // Assert
    assert_eq!(
        chat.instruction_placement,
        InstructionPlacement::JoinPreviousUser
    );
    assert!(chat.validate().is_ok() && separate.validate().is_ok());
    assert!(
        matches!(
            &result,
            Err(ModelError::InvalidProviderConfig { reason })
                if reason.contains("cannot express instructions placed other than as their own message")
        ),
        "{result:?}"
    );
}

#[test]
fn thinking_options_default_to_thinking_on_without_preserved_reasoning() {
    // Arrange / Act
    let options = ThinkingOptions::default()
        .with_disable_supported(true)
        .with_preserve_reasoning(true);

    // Assert
    assert!(!ThinkingOptions::default().disable_supported);
    assert!(!ThinkingOptions::default().preserve_reasoning);
    assert!(options.disable_supported && options.preserve_reasoning);
}

#[test]
fn for_model_restricts_the_specification_to_one_model() {
    // Arrange / Act
    let spec = chat().for_model("llama3.3");

    // Assert
    assert_eq!(chat().model(), None);
    assert_eq!(spec.model(), Some("llama3.3"));
}

#[test]
fn validates_schema_input_and_output_bound() {
    // Arrange
    let array = OutputSchema::new(json!({"type": "array"})).expect("valid schema");
    let bound = |tokens| GenerationOptions {
        max_output_tokens: NonZeroU32::new(tokens),
        ..GenerationOptions::default()
    };
    let unbounded = chat().with_max_tokens_field(None);

    // Act
    let schema_error = chat().with_display_name("Gateway").validate_schema(&array);
    let image_error = chat().validate_input("llama3.3", &image_input());
    let image_ok = chat()
        .with_image_input(true)
        .validate_input("llava", &image_input());

    // Assert
    assert!(matches!(
        schema_error,
        Err(ModelError::UnsupportedOutputSchema { reason })
            if reason == "Gateway structured output requires an explicit object root schema"
    ));
    assert!(matches!(
        image_error,
        Err(ModelError::UnsupportedImageInput { reason })
            if reason == "custom configuration `llama3.3` does not accept image content"
    ));
    assert!(image_ok.is_ok());
    assert!(chat().validate_generation(&bound(1)).is_ok());
    assert!(unbounded.validate_generation(&bound(0)).is_ok());
    assert!(matches!(
        unbounded.validate_generation(&bound(1)),
        Err(ModelError::UnsupportedGeneration { .. })
    ));
    assert!(matches!(
        responses().validate_generation(&bound(15)),
        Err(ModelError::UnsupportedGeneration { .. })
    ));
    assert!(responses().validate_generation(&bound(16)).is_ok());
}

#[test]
fn debug_output_names_the_auth_without_secrets() {
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
        (Auth::Credential(Arc::new(StubCredential)), "Credential"),
    ];

    // Act / Assert
    for (auth, expected) in auths {
        assert_eq!(format!("{auth:?}"), expected);
    }
}

#[test]
fn debug_output_names_headers_and_query_parameters_without_values() {
    // Arrange
    let spec = chat()
        .with_auth(Auth::Bearer("bearer-secret".to_string()))
        .with_header("x-gateway-key", "header-secret")
        .with_query("key", "query-secret");

    // Act
    let debug = format!("{spec:?}");

    // Assert
    assert!(debug.contains("x-gateway-key: <redacted>"), "{debug}");
    assert!(debug.contains("key: <redacted>"), "{debug}");
    assert!(!debug.contains("secret"), "{debug}");
}

#[test]
fn neither_debug_output_nor_validation_reveals_base_url_secrets() {
    // Arrange
    let specs = [
        "https://gw.example/v1?api-key=url-secret#frag-secret",
        "https://user:url-secret@gw.example/v1",
        "not a url-secret",
    ]
    .map(|url| ProviderSpec::new("custom", WireProtocol::ChatCompletions, url));

    // Act
    let outputs = specs.map(|spec| {
        let reason = spec.validate().map_err(|error| error.to_string());

        (format!("{spec:?}"), reason)
    });

    // Assert
    for (debug, reason) in &outputs {
        assert!(!debug.contains("secret"), "{debug}");
        let reason = reason.as_ref().expect_err("each base URL is invalid");
        assert!(!reason.contains("secret"), "{reason}");
    }
    assert!(outputs[0].0.contains("base_url: \"https://gw.example/v1\""));
    assert!(outputs[2].0.contains("base_url: \"<invalid>\""));
}
