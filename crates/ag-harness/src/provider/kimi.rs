use crate::model::ReasoningEffort;
use crate::{chat_completion, telemetry};

pub(crate) const KIMI_API_KEY_ENV: &str = "KIMI_API_KEY";
pub(crate) const KIMI_BASE_URL_ENV: &str = "KIMI_BASE_URL";

/// Kimi K2.6 model identifier.
pub const KIMI_K2_6: &str = "kimi-k2.6";

pub(crate) fn policy(model: &str) -> chat_completion::ChatCompletionProviderPolicy {
    chat_completion::ChatCompletionProviderPolicy {
        display_name: "Kimi",
        reasoning_format: match model {
            "kimi-k2.6" => chat_completion::ReasoningFormat::Thinking {
                disable_supported: true,
                preserve_reasoning: true,
            },
            "kimi-k2.7-code" => chat_completion::ReasoningFormat::Thinking {
                disable_supported: false,
                preserve_reasoning: false,
            },
            "kimi-k3" => chat_completion::ReasoningFormat::Effort(reasoning_effort_name),
            _ => chat_completion::ReasoningFormat::None,
        },
        response_format_with_tools: false,
        structured_output: chat_completion::StructuredOutputMode::JsonObject {
            assistant_reasoning_content: matches!(
                model,
                "kimi-k2.6" | "kimi-k2.7-code" | "kimi-k3"
            ),
            tool_result_name: true,
        },
        telemetry_name: telemetry::PROVIDER_MOONSHOT_AI,
        unsupported_schema_reason: "Kimi JSON Object mode requires an explicit object root schema",
    }
}

fn reasoning_effort_name(reasoning_effort: ReasoningEffort) -> &'static str {
    match reasoning_effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium | ReasoningEffort::High => "high",
        ReasoningEffort::XHigh | ReasoningEffort::Max => "max",
    }
}

/// Configuration for a Kimi model served through Moonshot AI's
/// OpenAI-compatible API.
pub struct KimiConfig {
    /// API key sent as a bearer token.
    pub api_key: String,
    /// API base URL ending in the OpenAI-compatible version path.
    pub base_url: String,
    /// Kimi model identifier sent with each request.
    pub model: String,
}

#[cfg(test)]
#[path = "kimi_test.rs"]
mod tests;
