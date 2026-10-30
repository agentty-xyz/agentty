use crate::model::{HeuristicContextEstimator, ModelProfile};

pub(crate) const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
pub(crate) const OPENAI_API_KEY_ENV: &str = "OPENAI_API_KEY";
pub(crate) const OPENAI_BASE_URL_ENV: &str = "OPENAI_BASE_URL";

/// GPT-6.1 Sol, the general-purpose model `OpenAI` recommends for Codex.
pub const GPT_6_1_SOL: &str = "gpt-6.1-sol";

/// GPT-6 Astra, `OpenAI`'s most capable model.
pub const GPT_6_ASTRA: &str = "gpt-6-astra";

/// GPT-6 Luna, `OpenAI`'s efficient model for high-volume tasks.
pub const GPT_6_LUNA: &str = "gpt-6-luna";

/// GPT-5.6 Terra, `OpenAI`'s balanced model.
pub const GPT_5_6_TERRA: &str = "gpt-5.6-terra";

/// GPT-5.3-Codex, the last Codex-specific model, deprecated by `OpenAI` in
/// favor of the general-purpose models above.
pub const GPT_5_3_CODEX: &str = "gpt-5.3-codex";

/// Percent by which the unfitted default estimator may under-count `OpenAI`
/// tokens. No `OpenAI` model has a fitted estimator yet, so this takes the
/// 50% that keeps unknown tokenizers inside their window; calibration from
/// reported usage then narrows the prediction.
const ESTIMATE_UNDER_COUNT_PERCENT: u64 = 50;
/// Output limit of every cataloged model.
const MAX_OUTPUT_TOKENS: u64 = 128_000;
/// GPT-6 family and GPT-5.6 Terra: documented 1,050,000-token window with a
/// 922,000-token maximum input and 128,000-token maximum output. The
/// estimate margin keeps requests well under the maximum input.
const GPT_6_FAMILY: ModelProfile = ModelProfile::new(1_050_000)
    .with_estimate_margin(MAX_OUTPUT_TOKENS, ESTIMATE_UNDER_COUNT_PERCENT)
    .with_estimator(OPENAI_ESTIMATOR);
/// GPT-5.3-Codex: documented 400,000-token window with a 272,000-token
/// maximum input and 128,000-token maximum output.
const GPT_5_3_CODEX_PROFILE: ModelProfile = ModelProfile::new(400_000)
    .with_estimate_margin(MAX_OUTPUT_TOKENS, ESTIMATE_UNDER_COUNT_PERCENT)
    .with_estimator(OPENAI_ESTIMATOR);
/// The Responses API renders the `json_schema` text format into the prompt
/// it constrains, so the output schema counts toward the request.
const OPENAI_ESTIMATOR: HeuristicContextEstimator =
    HeuristicContextEstimator::DEFAULT.with_output_schema_in_prompt();

/// Configuration for an `OpenAI` model served through the Responses API.
pub struct OpenAiConfig {
    /// API key sent as a bearer token.
    pub api_key: String,
    /// API base URL ending in the version path, such as
    /// `https://api.openai.com/v1`.
    pub base_url: String,
    /// `OpenAI` model identifier sent with each request.
    pub model: String,
}

/// Returns the documented context profile of a cataloged `OpenAI` model, or
/// `None` for a model this catalog does not list.
pub(crate) fn profile(model: &str) -> Option<ModelProfile> {
    match model {
        GPT_6_1_SOL | GPT_6_ASTRA | GPT_6_LUNA | GPT_5_6_TERRA => Some(GPT_6_FAMILY),
        GPT_5_3_CODEX => Some(GPT_5_3_CODEX_PROFILE),
        _ => None,
    }
}

#[cfg(test)]
#[path = "openai_test.rs"]
mod tests;
