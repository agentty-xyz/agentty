//! Built-in preset policy tests: per-model reasoning, structured-output,
//! image, and output-bound choices.

use std::error::Error;
use std::sync::Arc;

use async_trait::async_trait;

use crate::credential::CredentialSource;
use crate::model::ReasoningEffort;
use crate::preset;
use crate::provider_spec::{
    Auth, InstructionPlacement, MaxTokensField, ProviderSpec, ReasoningMapping,
    StructuredOutputMode, ThinkingOptions, WireProtocol,
};

const BASE_URL: &str = "https://example.com/v1";

struct StubCredential;

#[async_trait]
impl CredentialSource for StubCredential {
    async fn bearer_token(&self) -> Result<String, Box<dyn Error + Send + Sync>> {
        Ok("stub-token".to_string())
    }
}

/// Returns the wire name `spec` sends for `effort`, or `None` when it maps
/// efforts some other way.
fn effort_name(spec: &ProviderSpec, effort: ReasoningEffort) -> Option<String> {
    match &spec.reasoning {
        ReasoningMapping::Effort(names) => Some(names.name(effort).to_string()),
        ReasoningMapping::None
        | ReasoningMapping::EnableThinking
        | ReasoningMapping::Thinking(_) => None,
    }
}

#[test]
fn selects_reasoning_and_structured_output_by_model() {
    // Arrange
    let kimi = preset::kimi(BASE_URL, "key", "kimi-k2.6");
    let kimi_code = preset::kimi(BASE_URL, "key", "kimi-k2.7-code");
    let kimi_k3 = preset::kimi(BASE_URL, "key", "kimi-k3");
    let kimi_legacy = preset::kimi(BASE_URL, "key", "kimi-legacy");
    let muse = preset::muse(BASE_URL, "key", "muse-spark-1.3");
    let qwen_plus = preset::qwen(BASE_URL, "key", "qwen-plus");
    let qwen_38 = preset::qwen(BASE_URL, "key", "qwen3.8-max");
    let qwen_max = preset::qwen(BASE_URL, "key", "qwen3-max");

    // Act / Assert
    assert_eq!(kimi.model(), Some("kimi-k2.6"));
    assert_eq!(
        kimi.reasoning,
        ReasoningMapping::Thinking(
            ThinkingOptions::default()
                .with_disable_supported(true)
                .with_preserve_reasoning(true)
        )
    );
    assert_eq!(
        kimi_code.reasoning,
        ReasoningMapping::Thinking(ThinkingOptions::default())
    );
    assert_eq!(kimi_legacy.reasoning, ReasoningMapping::None);
    assert_eq!(
        effort_name(&kimi_k3, ReasoningEffort::Max).as_deref(),
        Some("max")
    );
    assert_eq!(
        effort_name(&kimi_k3, ReasoningEffort::Medium).as_deref(),
        Some("high")
    );
    assert_eq!(
        effort_name(&muse, ReasoningEffort::Max).as_deref(),
        Some("xhigh")
    );
    assert_eq!(
        effort_name(&muse, ReasoningEffort::Medium).as_deref(),
        Some("medium")
    );
    assert_eq!(qwen_plus.reasoning, ReasoningMapping::EnableThinking);
    assert_eq!(qwen_max.reasoning, ReasoningMapping::None);
    assert_eq!(
        effort_name(&qwen_38, ReasoningEffort::High).as_deref(),
        Some("xhigh")
    );
    assert_eq!(
        effort_name(&qwen_38, ReasoningEffort::Low).as_deref(),
        Some("low")
    );
    assert_eq!(muse.structured_output, StructuredOutputMode::JsonSchema);
    assert_eq!(kimi.structured_output, StructuredOutputMode::JsonObject);
    assert!(kimi.replay.reasoning_text && kimi.replay.tool_result_name);
    assert_eq!(
        qwen_plus.structured_output,
        StructuredOutputMode::JsonObject
    );
    assert!(!qwen_plus.replay.reasoning_text && !qwen_plus.replay.tool_result_name);
    assert!(qwen_38.replay.reasoning_text);
    assert!(!qwen_plus.response_format_with_tools);
    assert!(muse.response_format_with_tools);
    assert_eq!(
        muse.max_tokens_field,
        Some(MaxTokensField::MaxCompletionTokens)
    );
    assert!(kimi.prompt_cache_key && muse.prompt_cache_key && !qwen_plus.prompt_cache_key);
}

#[test]
fn chat_completions_presets_opt_in_to_their_compat_quirks() {
    // Arrange
    // The golden requests place instructions after a tool result, where both
    // placements send the same messages, so the opt-in is pinned here.
    let cases = [
        (
            preset::kimi(BASE_URL, "key", "kimi-k3"),
            MaxTokensField::MaxTokens,
        ),
        (
            preset::muse(BASE_URL, "key", "muse-spark-1.3"),
            MaxTokensField::MaxCompletionTokens,
        ),
        (
            preset::openai_compatible(BASE_URL, None),
            MaxTokensField::MaxTokens,
        ),
        (
            preset::qwen(BASE_URL, "key", "qwen3.8-max"),
            MaxTokensField::MaxTokens,
        ),
    ];

    for (spec, field) in cases {
        // Act / Assert
        assert_eq!(
            spec.instruction_placement,
            InstructionPlacement::JoinPreviousUser,
            "{}",
            spec.id()
        );
        assert_eq!(spec.max_tokens_field, Some(field), "{}", spec.id());
    }
}

#[test]
fn openai_compatible_sends_only_portable_fields() {
    // Arrange / Act
    let keyless = preset::openai_compatible(BASE_URL, None);
    let keyed = preset::openai_compatible(BASE_URL, Some("key".to_string()));

    // Assert
    assert_eq!(keyless.id(), "openai-compatible");
    assert_eq!(keyless.model(), None);
    assert_eq!(keyless.protocol, WireProtocol::ChatCompletions);
    assert!(matches!(keyless.auth, Auth::None));
    assert!(matches!(keyed.auth, Auth::Bearer(ref key) if key == "key"));
    assert!(!keyless.prompt_cache_key);
    assert!(!keyless.image_input);
    assert!(!keyless.response_format_with_tools);
    assert_eq!(keyless.reasoning, ReasoningMapping::None);
    assert_eq!(
        keyless.structured_output,
        StructuredOutputMode::SchemaPrompt
    );
    assert_eq!(keyless.max_tokens_field, Some(MaxTokensField::MaxTokens));
}

#[test]
fn openai_maps_max_effort_by_model_family() {
    // Arrange
    let current = ["gpt-6.1-sol", "gpt-6-astra", "gpt-6-luna", "gpt-5.6-terra"];
    let earlier = ["gpt-5.3-codex", "gpt-5.5"];

    // Act / Assert
    for model in current {
        let spec = preset::openai(BASE_URL, "key", model);
        assert_eq!(
            effort_name(&spec, ReasoningEffort::Max).as_deref(),
            Some("max"),
            "{model}"
        );
        assert_eq!(
            effort_name(&spec, ReasoningEffort::XHigh).as_deref(),
            Some("xhigh"),
            "{model}"
        );
    }
    for model in earlier {
        let spec = preset::openai(BASE_URL, "key", model);
        assert_eq!(
            effort_name(&spec, ReasoningEffort::Max).as_deref(),
            Some("xhigh"),
            "{model}"
        );
        assert_eq!(
            effort_name(&spec, ReasoningEffort::Low).as_deref(),
            Some("low"),
            "{model}"
        );
    }
}

#[test]
fn chatgpt_names_the_plan_and_rejects_an_output_bound() {
    // Arrange / Act
    let chatgpt = preset::chatgpt(BASE_URL, Arc::new(StubCredential), "gpt-6-luna");
    let openai = preset::openai(BASE_URL, "key", "gpt-6-luna");

    // Assert
    assert_eq!(chatgpt.id(), "chatgpt");
    assert_eq!(chatgpt.display_name, "ChatGPT plan");
    assert_eq!(openai.display_name, "OpenAI");
    assert!(matches!(chatgpt.auth, Auth::Credential(_)));
    assert_eq!(chatgpt.max_tokens_field, None);
    assert_eq!(
        openai.max_tokens_field,
        Some(MaxTokensField::MaxOutputTokens)
    );
    assert_eq!(
        effort_name(&chatgpt, ReasoningEffort::Max).as_deref(),
        Some("max")
    );
    assert!(chatgpt.image_input && openai.image_input);
}

#[test]
fn qualifies_image_input_by_model_name() {
    // Arrange
    let cases = [
        (preset::kimi(BASE_URL, "key", "kimi-k2.6"), true),
        (preset::kimi(BASE_URL, "key", "kimi-legacy"), false),
        (
            preset::muse(BASE_URL, "key", "muse-spark-1.3-contributor"),
            true,
        ),
        (preset::muse(BASE_URL, "key", "muse-legacy"), false),
        (preset::qwen(BASE_URL, "key", "qwen-vl-plus"), true),
        (preset::qwen(BASE_URL, "key", "qwen3-vl-plus"), true),
        (preset::qwen(BASE_URL, "key", "qwen-plus"), true),
        (preset::qwen(BASE_URL, "key", "qwen3-max"), false),
    ];

    // Act / Assert
    for (spec, accepts_images) in cases {
        assert_eq!(spec.image_input, accepts_images, "{spec:?}");
    }
}
