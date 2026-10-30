use crate::model::{HeuristicContextEstimator, ModelProfile};

pub(crate) const DASHSCOPE_API_KEY_ENV: &str = "DASHSCOPE_API_KEY";
pub(crate) const DASHSCOPE_BASE_URL_ENV: &str = "DASHSCOPE_BASE_URL";

/// Qwen3.8 Max model identifier.
pub const QWEN3_8_MAX: &str = "qwen3.8-max";

/// Qwen Plus model identifier.
pub const QWEN_PLUS: &str = "qwen-plus";

/// Qwen3.8 Flash, the current low-cost Qwen model for simple, high-volume
/// tasks.
pub const QWEN3_8_FLASH: &str = "qwen3.8-flash";

/// Qwen3.7 Flash, the lowest-cost Qwen model, billed cheapest for requests
/// under 32,000 input tokens.
pub const QWEN3_7_FLASH: &str = "qwen3.7-flash";

/// Percent by which the unfitted default estimator may under-count Qwen
/// flash tokens. The flash models have no fitted estimator yet, so this takes
/// the 50% that keeps unknown tokenizers inside their window.
const ESTIMATE_UNDER_COUNT_PERCENT: u64 = 50;
/// Qwen3.8 Flash and Qwen3.7 Flash: documented 1,000,000-token window,
/// 991,808-token maximum input, and 131,072-token maximum output. The
/// harness states the output schema in the prompt.
const QWEN_FLASH: ModelProfile = ModelProfile::new(1_000_000)
    .with_estimate_margin(131_072, ESTIMATE_UNDER_COUNT_PERCENT)
    .with_estimator(HeuristicContextEstimator::DEFAULT.with_output_schema_in_prompt());

/// Returns the documented context profile of a Qwen flash model, or `None`
/// for the models the host profiles with fitted estimators.
pub(crate) fn profile(model: &str) -> Option<ModelProfile> {
    match model {
        QWEN3_8_FLASH | QWEN3_7_FLASH => Some(QWEN_FLASH),
        _ => None,
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
