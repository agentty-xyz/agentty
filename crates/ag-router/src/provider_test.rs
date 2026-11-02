use std::error::Error;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use super::{Provider, ProviderConfig, ProviderPolicy, Router, policy};
use crate::chat_completion::{ChatCompletionProviderPolicy, ReasoningFormat, StructuredOutputMode};
use crate::credential::BearerCredential;
use crate::input::{ImageContent, ImageMediaType, InputBlock, TurnInput};
use crate::model::{ModelError, ReasoningEffort};
use crate::responses::ResponsesProviderPolicy;
use crate::schema::OutputSchema;

fn chat_policy(provider: Provider, model: &str) -> ChatCompletionProviderPolicy {
    match policy(provider, model) {
        ProviderPolicy::ChatCompletion(policy) => policy,
        ProviderPolicy::Responses(_) => {
            std::panic::resume_unwind(Box::new(format!("{model} should use Chat Completions")))
        }
    }
}

fn responses_policy(provider: Provider, model: &str) -> ResponsesProviderPolicy {
    match policy(provider, model) {
        ProviderPolicy::Responses(policy) => policy,
        ProviderPolicy::ChatCompletion(_) => {
            std::panic::resume_unwind(Box::new(format!("{model} should use Responses")))
        }
    }
}

fn router(provider: Provider) -> Router {
    Router::new([ProviderConfig {
        provider,
        api_key: Some("test-key".to_string()),
        base_url: "https://example.com/v1".to_string(),
        credential: None,
    }])
    .expect("valid provider config")
}

#[test]
fn rejects_duplicate_or_blank_provider_config() {
    // Arrange
    let config = || ProviderConfig {
        provider: Provider::Muse,
        api_key: Some("test-key".to_string()),
        base_url: "https://example.com/v1".to_string(),
        credential: None,
    };

    // Act / Assert
    assert!(matches!(
        Router::new([config(), config()]),
        Err(ModelError::DuplicateProvider)
    ));
    let schema = OutputSchema::new(json!({"type":"object"})).expect("valid schema");
    let blank_key = Router::new([ProviderConfig {
        api_key: Some(" ".to_string()),
        ..config()
    }])
    .expect("single provider is accepted");
    let missing_key = Router::new([ProviderConfig {
        api_key: None,
        ..config()
    }])
    .expect("single provider is accepted");
    assert!(matches!(
        missing_key.validate_schema("muse/muse-spark-1.3", &schema),
        Err(ModelError::InvalidProviderConfig)
    ));
    let blank_url = Router::new([ProviderConfig {
        base_url: " ".to_string(),
        ..config()
    }])
    .expect("single provider is accepted");
    assert!(matches!(
        blank_key.validate_schema("muse/muse-spark-1.3", &schema),
        Err(ModelError::InvalidProviderConfig)
    ));
    assert!(matches!(
        blank_url.validate_schema("muse/muse-spark-1.3", &schema),
        Err(ModelError::InvalidProviderConfig)
    ));
}

#[test]
fn selects_provider_policies_and_reasoning_wire_values() {
    // Arrange
    let kimi = chat_policy(Provider::Kimi, "kimi-k2.6");
    let kimi_code = chat_policy(Provider::Kimi, "kimi-k2.7-code");
    let kimi_k3 = chat_policy(Provider::Kimi, "kimi-k3");
    let muse = chat_policy(Provider::Muse, "muse-spark-1.3");
    let qwen_plus = chat_policy(Provider::Qwen, "qwen-plus");
    let qwen_38 = chat_policy(Provider::Qwen, "qwen3.8-max");

    // Act / Assert
    assert!(matches!(
        kimi.reasoning_format,
        ReasoningFormat::Thinking {
            disable_supported: true,
            preserve_reasoning: true
        }
    ));
    assert!(matches!(
        kimi_code.reasoning_format,
        ReasoningFormat::Thinking {
            disable_supported: false,
            preserve_reasoning: false
        }
    ));
    assert!(
        matches!(kimi_k3.reasoning_format, ReasoningFormat::Effort(name) if name(ReasoningEffort::Max) == "max")
    );
    assert!(
        matches!(kimi_k3.reasoning_format, ReasoningFormat::Effort(name)
        if name(ReasoningEffort::Low) == "low" && name(ReasoningEffort::Medium) == "high")
    );
    assert!(
        matches!(muse.reasoning_format, ReasoningFormat::Effort(name) if name(ReasoningEffort::Max) == "xhigh")
    );
    assert!(matches!(
        qwen_plus.reasoning_format,
        ReasoningFormat::EnableThinking
    ));
    assert!(
        matches!(qwen_38.reasoning_format, ReasoningFormat::Effort(name) if name(ReasoningEffort::High) == "xhigh")
    );
    assert!(
        matches!(qwen_38.reasoning_format, ReasoningFormat::Effort(name)
        if name(ReasoningEffort::Low) == "low" && name(ReasoningEffort::Medium) == "medium")
    );
    assert!(matches!(
        muse.structured_output,
        StructuredOutputMode::JsonSchema
    ));
    assert!(matches!(
        kimi.structured_output,
        StructuredOutputMode::JsonObject {
            tool_result_name: true,
            ..
        }
    ));
    assert!(!qwen_plus.response_format_with_tools);
    assert!(muse.response_format_with_tools);
}

#[test]
fn openai_compatible_policy_sends_only_portable_fields() {
    // Arrange
    let model = "qwen3:latest";

    // Act
    let generic = chat_policy(Provider::OpenAiCompatible, model);

    // Assert
    assert_eq!(Provider::OpenAiCompatible.as_str(), "openai-compatible");
    assert!(!generic.prompt_cache_key);
    assert!(!generic.image_input);
    assert!(!generic.response_format_with_tools);
    assert!(matches!(generic.reasoning_format, ReasoningFormat::None));
    assert!(matches!(
        generic.structured_output,
        StructuredOutputMode::SchemaPrompt
    ));
}

#[test]
fn openai_policy_maps_max_effort_by_model_family() {
    // Arrange
    let current = ["gpt-6.1-sol", "gpt-6-astra", "gpt-6-luna", "gpt-5.6-terra"];
    let earlier = ["gpt-5.3-codex", "gpt-5.5"];

    // Act / Assert
    assert_eq!(Provider::OpenAi.as_str(), "openai");
    for model in current {
        let policy = responses_policy(Provider::OpenAi, model);
        assert_eq!(
            (policy.reasoning_effort)(ReasoningEffort::Max),
            "max",
            "{model}"
        );
        assert_eq!(
            (policy.reasoning_effort)(ReasoningEffort::XHigh),
            "xhigh",
            "{model}"
        );
    }
    for model in earlier {
        let policy = responses_policy(Provider::OpenAi, model);
        assert_eq!(
            (policy.reasoning_effort)(ReasoningEffort::Max),
            "xhigh",
            "{model}"
        );
        assert_eq!(
            (policy.reasoning_effort)(ReasoningEffort::Low),
            "low",
            "{model}"
        );
    }
}

#[test]
fn openai_requires_an_api_key_and_an_object_schema() {
    // Arrange
    let object = OutputSchema::new(json!({"type":"object"})).expect("valid schema");
    let array = OutputSchema::new(json!({"type":"array"})).expect("valid array schema");
    let config = |api_key: Option<&str>| ProviderConfig {
        provider: Provider::OpenAi,
        api_key: api_key.map(str::to_string),
        base_url: "https://api.openai.com/v1".to_string(),
        credential: None,
    };
    let image = ImageContent::new(
        ImageMediaType::Png,
        vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
    )
    .expect("valid image");
    let input = TurnInput::new(vec![InputBlock::Image(image)]).expect("valid input");

    // Act
    let keyed = Router::new([config(Some("sk-test"))]).expect("valid provider config");
    let keyless = Router::new([config(None)]).expect("single provider is accepted");

    // Assert
    assert!(keyed.validate_schema("openai/gpt-6.1-sol", &object).is_ok());
    assert!(keyed.validate_input("openai/gpt-6.1-sol", &input).is_ok());
    assert!(matches!(
        keyed.validate_schema("openai/gpt-6.1-sol", &array),
        Err(ModelError::UnsupportedOutputSchema { .. })
    ));
    assert_eq!(
        keyed.keeps_prefix_across_schemas("openai/gpt-6.1-sol").ok(),
        Some(false)
    );
    assert!(matches!(
        keyless.validate_schema("openai/gpt-6.1-sol", &object),
        Err(ModelError::InvalidProviderConfig)
    ));
}

#[test]
fn openai_compatible_accepts_a_missing_but_not_a_blank_api_key() {
    // Arrange
    let schema = OutputSchema::new(json!({"type":"object"})).expect("valid schema");
    let config = |api_key: Option<&str>| ProviderConfig {
        provider: Provider::OpenAiCompatible,
        api_key: api_key.map(str::to_string),
        base_url: "http://127.0.0.1:11434/v1".to_string(),
        credential: None,
    };

    // Act
    let unauthenticated = Router::new([config(None)]).expect("valid provider config");
    let blank = Router::new([config(Some(" "))]).expect("single provider is accepted");

    // Assert
    assert!(
        unauthenticated
            .validate_schema("openai-compatible/qwen3:latest", &schema)
            .is_ok()
    );
    assert!(matches!(
        blank.validate_schema("openai-compatible/qwen3:latest", &schema),
        Err(ModelError::InvalidProviderConfig)
    ));
}

#[test]
fn qualifies_image_input_by_model_name() {
    // Arrange
    let cases = [
        (Provider::Kimi, "kimi-k2.6", true),
        (Provider::Kimi, "kimi-legacy", false),
        (Provider::Muse, "muse-spark-1.3-contributor", true),
        (Provider::Muse, "muse-legacy", false),
        (Provider::Qwen, "qwen-vl-plus", true),
        (Provider::Qwen, "qwen3-vl-plus", true),
        (Provider::Qwen, "qwen-plus", true),
        (Provider::Qwen, "qwen3-max", false),
    ];

    // Act / Assert
    for (provider, model, accepts_images) in cases {
        assert_eq!(
            chat_policy(provider, model).image_input,
            accepts_images,
            "{model}"
        );
    }
}

#[test]
fn rejects_unsupported_schema_and_image_before_request() {
    // Arrange
    let router = router(Provider::Muse);
    let schema = OutputSchema::new(json!({"type":"array"})).expect("valid array schema");
    let image = ImageContent::new(
        ImageMediaType::Png,
        vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
    )
    .expect("valid image");
    let input = TurnInput::new(vec![InputBlock::Image(image)]).expect("valid input");

    // Act / Assert
    assert!(matches!(
        router.validate_schema("muse/muse-spark-1.3", &schema),
        Err(ModelError::UnsupportedOutputSchema { .. })
    ));
    assert!(matches!(
        router.validate_input("muse/muse-legacy", &input),
        Err(ModelError::UnsupportedImageInput { .. })
    ));
    assert!(router.validate_input("muse/muse-spark-1.3", &input).is_ok());
}

#[test]
fn rejects_malformed_model_ids() {
    // Arrange
    let router = router(Provider::Muse);
    let schema = OutputSchema::new(json!({"type":"object"})).expect("valid schema");

    // Act / Assert
    for id in ["muse", "muse/", "/model"] {
        assert!(
            matches!(
                router.validate_schema(id, &schema),
                Err(ModelError::InvalidModelId)
            ),
            "{id}"
        );
    }
    assert!(matches!(
        router.validate_schema("other/model", &schema),
        Err(ModelError::UnknownProvider)
    ));
}

struct StubCredential;

#[async_trait]
impl BearerCredential for StubCredential {
    async fn bearer_token(&self) -> Result<String, Box<dyn Error + Send + Sync>> {
        Ok("stub-token".to_string())
    }
}

fn credential_config(provider: Provider, base_url: &str) -> ProviderConfig {
    ProviderConfig {
        provider,
        api_key: None,
        base_url: base_url.to_string(),
        credential: Some(Arc::new(StubCredential)),
    }
}

#[test]
fn chatgpt_requires_a_credential_and_validates_like_openai() {
    // Arrange
    let object = OutputSchema::new(json!({"type":"object"})).expect("valid schema");
    let array = OutputSchema::new(json!({"type":"array"})).expect("valid array schema");
    let input = TurnInput::new(vec![InputBlock::Image(
        ImageContent::new(
            ImageMediaType::Png,
            vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
        )
        .expect("valid image"),
    )])
    .expect("valid input");
    let model = "chatgpt/gpt-6-luna";

    // Act
    let keyed_without_credential = Router::new([ProviderConfig {
        provider: Provider::ChatGpt,
        api_key: Some("sk-test".to_string()),
        base_url: "https://api.openai.com/v1".to_string(),
        credential: None,
    }])
    .expect("single provider is accepted");
    let signed_in = Router::new([credential_config(
        Provider::ChatGpt,
        "https://api.openai.com/v1",
    )])
    .expect("valid provider config");
    let blank_url =
        Router::new([credential_config(Provider::ChatGpt, " ")]).expect("single provider");

    // Assert
    assert_eq!(Provider::ChatGpt.as_str(), "chatgpt");
    assert!(matches!(
        keyed_without_credential.validate_schema(model, &object),
        Err(ModelError::InvalidProviderConfig)
    ));
    assert!(matches!(
        blank_url.validate_schema(model, &object),
        Err(ModelError::InvalidProviderConfig)
    ));
    assert!(signed_in.validate_schema(model, &object).is_ok());
    assert!(signed_in.validate_input(model, &input).is_ok());
    assert_eq!(
        signed_in.keeps_prefix_across_schemas(model).ok(),
        Some(false)
    );
    assert!(matches!(
        signed_in.validate_schema(model, &array),
        Err(ModelError::UnsupportedOutputSchema { .. })
    ));
}

#[test]
fn credential_replaces_the_api_key_for_other_providers() {
    // Arrange
    let schema = OutputSchema::new(json!({"type":"object"})).expect("valid schema");

    // Act
    let router = Router::new([credential_config(
        Provider::OpenAi,
        "https://api.openai.com/v1",
    )])
    .expect("valid provider config");

    // Assert
    assert!(router.validate_schema("openai/gpt-6-luna", &schema).is_ok());
}

#[test]
fn chatgpt_policy_names_the_plan_and_maps_effort_like_openai() {
    // Arrange
    let model = "gpt-6-luna";

    // Act
    let chatgpt = responses_policy(Provider::ChatGpt, model);
    let openai = responses_policy(Provider::OpenAi, model);

    // Assert
    assert_eq!(chatgpt.display_name, "ChatGPT plan");
    assert_eq!(openai.display_name, "OpenAI");
    assert_eq!((chatgpt.reasoning_effort)(ReasoningEffort::Max), "max");
}
