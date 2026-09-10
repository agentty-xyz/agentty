use crate::model::ReasoningEffort;
use crate::{chat_completion, telemetry};

pub(crate) const DASHSCOPE_API_KEY_ENV: &str = "DASHSCOPE_API_KEY";
pub(crate) const DASHSCOPE_BASE_URL_ENV: &str = "DASHSCOPE_BASE_URL";

/// Qwen Plus model identifier.
pub const QWEN_PLUS: &str = "qwen-plus";

pub(crate) fn policy(model: &str) -> chat_completion::ChatCompletionProviderPolicy {
    let preserves_reasoning = model.starts_with("qwen3.8-");

    chat_completion::ChatCompletionProviderPolicy {
        display_name: "Qwen",
        reasoning_format: if preserves_reasoning {
            chat_completion::ReasoningFormat::Effort(reasoning_effort_name)
        } else if model == QWEN_PLUS {
            chat_completion::ReasoningFormat::EnableThinking
        } else {
            chat_completion::ReasoningFormat::None
        },
        response_format_with_tools: false,
        structured_output: chat_completion::StructuredOutputMode::JsonObject {
            assistant_reasoning_content: preserves_reasoning,
            tool_result_name: false,
        },
        telemetry_name: telemetry::PROVIDER_ALIBABA_CLOUD,
        unsupported_schema_reason: "Qwen JSON Object mode requires an explicit object root schema",
    }
}

fn reasoning_effort_name(reasoning_effort: ReasoningEffort) -> &'static str {
    match reasoning_effort {
        ReasoningEffort::Low | ReasoningEffort::Medium => reasoning_effort.as_str(),
        ReasoningEffort::High | ReasoningEffort::XHigh | ReasoningEffort::Max => {
            ReasoningEffort::XHigh.as_str()
        }
    }
}

/// Configuration for a Qwen model served through Alibaba Cloud Model Studio's
/// OpenAI-compatible API.
pub struct QwenConfig {
    /// API key sent as a bearer token.
    pub api_key: String,
    /// API base URL ending in the OpenAI-compatible version path.
    pub base_url: String,
    /// Qwen model identifier sent with each request.
    pub model: String,
}

#[cfg(test)]
#[path = "qwen_test.rs"]
mod tests;
