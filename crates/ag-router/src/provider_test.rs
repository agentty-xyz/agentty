use serde_json::json;

use super::{Provider, ProviderConfig, Router, policy};
use crate::chat_completion::{ReasoningFormat, StructuredOutputMode};
use crate::input::{ImageContent, ImageMediaType, InputBlock, TurnInput};
use crate::model::{ModelError, ReasoningEffort};
use crate::schema::OutputSchema;

fn router(provider: Provider) -> Router {
    Router::new([ProviderConfig {
        provider,
        api_key: "test-key".to_string(),
        base_url: "https://example.com/v1".to_string(),
    }])
    .expect("valid provider config")
}

#[test]
fn rejects_duplicate_or_blank_provider_config() {
    // Arrange
    let config = || ProviderConfig {
        provider: Provider::Muse,
        api_key: "test-key".to_string(),
        base_url: "https://example.com/v1".to_string(),
    };

    // Act / Assert
    assert!(matches!(
        Router::new([config(), config()]),
        Err(ModelError::DuplicateProvider)
    ));
    let schema = OutputSchema::new(json!({"type":"object"})).expect("valid schema");
    let blank_key = Router::new([ProviderConfig {
        api_key: " ".to_string(),
        ..config()
    }])
    .expect("single provider is accepted");
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
    let kimi = policy(Provider::Kimi, "kimi-k2.6");
    let kimi_code = policy(Provider::Kimi, "kimi-k2.7-code");
    let kimi_k3 = policy(Provider::Kimi, "kimi-k3");
    let muse = policy(Provider::Muse, "muse-spark-1.3");
    let qwen_plus = policy(Provider::Qwen, "qwen-plus");
    let qwen_38 = policy(Provider::Qwen, "qwen3.8-max");

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
            policy(provider, model).image_input,
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
