//! Router tests: specification routing, replacement, and request validation
//! before any network access.

use std::error::Error;
use std::num::NonZeroU32;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use super::Router;
use crate::credential::CredentialSource;
use crate::input::{ImageContent, ImageMediaType, InputBlock, TurnInput};
use crate::model::{ErrorClass, GenerationOptions, ModelError};
use crate::preset;
use crate::provider_spec::{Auth, ProviderSpec, WireProtocol};
use crate::schema::OutputSchema;

const BASE_URL: &str = "https://example.com/v1";

struct StubCredential;

#[async_trait]
impl CredentialSource for StubCredential {
    async fn bearer_token(&self) -> Result<String, Box<dyn Error + Send + Sync>> {
        Ok("stub-token".to_string())
    }
}

fn object_schema() -> OutputSchema {
    OutputSchema::new(json!({"type":"object"})).expect("valid schema")
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
fn routes_each_model_to_its_own_preset_and_rejects_an_unconfigured_one() {
    // Arrange
    let router = Router::new([
        preset::kimi(BASE_URL, "key", "kimi-k2.6"),
        preset::kimi(BASE_URL, "key", "kimi-legacy"),
    ]);

    // Act / Assert
    assert!(
        router
            .validate_input("kimi/kimi-k2.6", &image_input())
            .is_ok()
    );
    assert!(matches!(
        router.validate_input("kimi/kimi-legacy", &image_input()),
        Err(ModelError::UnsupportedImageInput { .. })
    ));
    assert!(matches!(
        router.validate_schema("kimi/kimi-k3", &object_schema()),
        Err(ModelError::NoProviderSpec)
    ));
}

#[test]
fn a_model_specific_specification_wins_over_one_for_every_model() {
    // Arrange
    let router = Router::new([
        ProviderSpec::new("gateway", WireProtocol::ChatCompletions, BASE_URL),
        ProviderSpec::new("gateway", WireProtocol::ChatCompletions, BASE_URL)
            .for_model("vision")
            .with_image_input(true),
    ]);

    // Act / Assert
    assert!(
        router
            .validate_input("gateway/vision", &image_input())
            .is_ok()
    );
    assert!(matches!(
        router.validate_input("gateway/other", &image_input()),
        Err(ModelError::UnsupportedImageInput { .. })
    ));
}

#[test]
fn an_override_replaces_the_specification_with_the_same_id_and_model() {
    // Arrange
    let array = OutputSchema::new(json!({"type":"array"})).expect("valid schema");
    let invalid =
        ProviderSpec::new("muse", WireProtocol::ChatCompletions, " ").for_model("muse-spark-1.3");

    // Act
    let replaced = Router::new([invalid.clone()]).with_override(preset::muse(
        BASE_URL,
        "key",
        "muse-spark-1.3",
    ));
    let replacing =
        Router::new([preset::muse(BASE_URL, "key", "muse-spark-1.3")]).with_override(invalid);

    // Assert
    assert!(
        replaced
            .validate_schema("muse/muse-spark-1.3", &object_schema())
            .is_ok()
    );
    assert!(matches!(
        replaced.validate_schema("muse/muse-spark-1.3", &array),
        Err(ModelError::UnsupportedOutputSchema { .. })
    ));
    assert!(matches!(
        replacing.validate_schema("muse/muse-spark-1.3", &object_schema()),
        Err(ModelError::InvalidProviderConfig { .. })
    ));
}

#[test]
fn reports_which_models_replay_plain_text_reasoning() {
    // Arrange
    let router = Router::new([
        preset::kimi(BASE_URL, "key", "kimi-k2.6"),
        preset::kimi(BASE_URL, "key", "kimi-k3"),
        preset::kimi(BASE_URL, "key", "kimi-legacy"),
        preset::qwen(BASE_URL, "key", "qwen3.8-max"),
        preset::qwen(BASE_URL, "key", "qwen-plus"),
        preset::muse(BASE_URL, "key", "muse-spark-1.3"),
        preset::openai_compatible(BASE_URL, None),
        preset::openai(BASE_URL, "key", "gpt-6.1-sol"),
        ProviderSpec::new("gateway", WireProtocol::ChatCompletions, BASE_URL)
            .with_reasoning_text_replay(true),
    ]);
    let cases = [
        ("kimi/kimi-k2.6", true),
        ("kimi/kimi-k3", true),
        ("kimi/kimi-legacy", false),
        ("qwen/qwen3.8-max", true),
        ("qwen/qwen-plus", false),
        ("muse/muse-spark-1.3", false),
        ("openai-compatible/qwen3:latest", false),
        ("openai/gpt-6.1-sol", false),
        ("gateway/any-model", true),
    ];

    // Act / Assert
    for (model, replays) in cases {
        assert_eq!(
            router.replays_reasoning_text(model).ok(),
            Some(replays),
            "{model}"
        );
    }
    assert!(router.replays_reasoning_text("muse/other").is_err());
}

#[test]
fn only_anthropic_messages_replays_turn_instructions() {
    // Arrange
    let router = Router::new([
        preset::anthropic(BASE_URL, "key", "claude-opus-5-5"),
        preset::openai(BASE_URL, "key", "gpt-6.1-sol"),
    ]);

    // Act
    let replays = [
        router.replays_turn_instructions("anthropic/claude-opus-5-5"),
        router.replays_turn_instructions("openai/gpt-6.1-sol"),
    ];

    // Assert
    assert_eq!(replays.map(Result::ok), [Some(true), Some(false)]);
    assert!(router.replays_turn_instructions("openai/other").is_err());
}

#[test]
fn keeps_the_first_of_duplicate_specifications() {
    // Arrange
    let model = "muse/muse-spark-1.3";

    // Act
    let router = Router::new([
        preset::muse(BASE_URL, "key", "muse-spark-1.3").with_image_input(false),
        preset::muse(BASE_URL, "other-key", "muse-spark-1.3").with_image_input(true),
    ]);

    // Assert
    assert!(matches!(
        router.validate_input(model, &image_input()),
        Err(ModelError::UnsupportedImageInput { .. })
    ));
}

#[test]
fn rejects_an_invalid_specification_on_every_validation() {
    // Arrange
    let router = Router::new([preset::kimi(BASE_URL, " ", "kimi-k3")]);
    let model = "kimi/kimi-k3";
    let invalid = |result: Result<(), ModelError>| {
        matches!(
            result,
            Err(ModelError::InvalidProviderConfig { reason }) if reason == "bearer key is blank"
        )
    };

    // Act / Assert
    assert!(invalid(router.validate_schema(model, &object_schema())));
    assert!(invalid(router.validate_input(model, &image_input())));
    assert!(invalid(
        router.validate_generation(model, &GenerationOptions::default())
    ));
    assert!(invalid(router.keeps_prefix_across_schemas(model).map(drop)));
    let error = ModelError::InvalidProviderConfig {
        reason: "bearer key is blank".to_string(),
    };
    assert_eq!(error.error_class(), ErrorClass::Request);
    assert_eq!(
        error.to_string(),
        "provider specification is invalid: bearer key is blank"
    );
}

#[test]
fn rejects_unsupported_schema_and_image_before_request() {
    // Arrange
    let router = Router::new([preset::muse(BASE_URL, "key", "muse-legacy")]);
    let images = Router::new([preset::muse(BASE_URL, "key", "muse-spark-1.3")]);
    let openai = Router::new([preset::openai(BASE_URL, "key", "gpt-6.1-sol")]);
    let array = OutputSchema::new(json!({"type":"array"})).expect("valid array schema");

    // Act / Assert
    assert!(matches!(
        router.validate_schema("muse/muse-legacy", &array),
        Err(ModelError::UnsupportedOutputSchema { .. })
    ));
    assert!(matches!(
        router.validate_input("muse/muse-legacy", &image_input()),
        Err(ModelError::UnsupportedImageInput { .. })
    ));
    assert!(
        images
            .validate_input("muse/muse-spark-1.3", &image_input())
            .is_ok()
    );
    assert!(
        openai
            .validate_input("openai/gpt-6.1-sol", &image_input())
            .is_ok()
    );
    assert_eq!(
        openai
            .keeps_prefix_across_schemas("openai/gpt-6.1-sol")
            .ok(),
        Some(false)
    );
}

#[test]
fn rejects_malformed_model_ids() {
    // Arrange
    let router = Router::new([preset::muse(BASE_URL, "key", "muse-spark-1.3")]);

    // Act / Assert
    for id in ["muse", "muse/", "/model"] {
        assert!(
            matches!(
                router.validate_schema(id, &object_schema()),
                Err(ModelError::InvalidModelId)
            ),
            "{id}"
        );
    }
    assert!(matches!(
        router.validate_schema("other/model", &object_schema()),
        Err(ModelError::NoProviderSpec)
    ));
}

#[test]
fn a_credential_authenticates_any_specification_without_a_key() {
    // Arrange
    let credential = || Auth::Credential(Arc::new(StubCredential));
    let router = Router::new([
        preset::openai(BASE_URL, "key", "gpt-6-luna").with_auth(credential()),
        preset::chatgpt(BASE_URL, Arc::new(StubCredential), "gpt-6-luna"),
    ]);

    // Act / Assert
    assert!(
        router
            .validate_schema("openai/gpt-6-luna", &object_schema())
            .is_ok()
    );
    assert!(
        router
            .validate_schema("chatgpt/gpt-6-luna", &object_schema())
            .is_ok()
    );
}

#[test]
fn validates_the_output_bound_each_preset_accepts() {
    // Arrange
    let bound = |tokens| GenerationOptions {
        max_output_tokens: NonZeroU32::new(tokens),
        ..GenerationOptions::default()
    };
    let router = Router::new([
        preset::chatgpt(BASE_URL, Arc::new(StubCredential), "gpt-6-luna"),
        preset::openai(BASE_URL, "key", "gpt-6-luna"),
        preset::kimi(BASE_URL, "key", "kimi-k3"),
    ]);
    let cases = [
        ("chatgpt/gpt-6-luna", 0, true),
        ("chatgpt/gpt-6-luna", 512, false),
        ("openai/gpt-6-luna", 0, true),
        ("openai/gpt-6-luna", 15, false),
        ("openai/gpt-6-luna", 16, true),
        ("kimi/kimi-k3", 1, true),
    ];

    for (model, tokens, accepted) in cases {
        // Act
        let result = router.validate_generation(model, &bound(tokens));

        // Assert
        assert_eq!(result.is_ok(), accepted, "{model} with {tokens}");
        if let Err(error) = result {
            assert!(
                matches!(error, ModelError::UnsupportedGeneration { .. }),
                "{model} with {tokens}: {error}"
            );
            assert_eq!(error.error_class(), ErrorClass::UnsupportedOutput);
        }
    }
}
