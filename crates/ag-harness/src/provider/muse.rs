//! Muse model identifiers, context profile, and configuration for Meta's
//! Model API.

use crate::model::{HeuristicContextEstimator, ModelProfile};

pub(crate) const DEFAULT_BASE_URL: &str = "https://api.meta.ai/v1";
pub(crate) const MODEL_API_BASE_URL_ENV: &str = "MODEL_API_BASE_URL";
pub(crate) const MODEL_API_KEY_ENV: &str = "MODEL_API_KEY";

/// Standard Muse Spark 1.3 model whose prompts and completions are not used
/// to train Meta models.
pub const MUSE_SPARK_1_3: &str = "muse-spark-1.3";

/// Discounted Muse Spark 1.3 model that permits Meta to use prompts and
/// completions to train future models.
pub const MUSE_SPARK_1_3_CONTRIBUTOR: &str = "muse-spark-1.3-contributor";

/// Output limit the Model API accepts; Meta documents none.
const MAX_OUTPUT_TOKENS: u64 = 131_072;
/// Percent by which the fitted Muse estimator may under-count: the largest
/// under-count it showed on recorded requests and on a live request near
/// the input budget, rounded up, plus five points.
const ESTIMATE_UNDER_COUNT_PERCENT: u64 = 18;
/// Muse Spark 1.3 and its contributor tier, which serves the same model:
/// documented 1,048,576-token window.
const MUSE_SPARK_1_3_PROFILE: ModelProfile = ModelProfile::new(1_048_576)
    .with_estimate_margin(MAX_OUTPUT_TOKENS, ESTIMATE_UNDER_COUNT_PERCENT)
    .with_estimator(MUSE_SPARK_1_3_ESTIMATOR);
/// Muse Spark 1.3, fitted to provider-reported input tokens: content at 4.4
/// bytes per token, tool definitions at 2.9, and a 420-token tool-calling
/// preamble. Muse counts the `json_schema` response format only beside
/// tools.
const MUSE_SPARK_1_3_ESTIMATOR: HeuristicContextEstimator = HeuristicContextEstimator::DEFAULT
    .with_messages(225, 44)
    .with_tool_definitions(339, 0)
    .with_request_tokens(420)
    .with_output_schema_in_prompt();

/// Returns the context profile of a cataloged Muse model, or `None` for a
/// model this catalog does not list.
pub(crate) fn profile(model: &str) -> Option<ModelProfile> {
    match model {
        MUSE_SPARK_1_3 | MUSE_SPARK_1_3_CONTRIBUTOR => Some(MUSE_SPARK_1_3_PROFILE),
        _ => None,
    }
}

/// Returns the output reservation of a cataloged Muse model, or `None` for
/// a model this catalog does not list.
pub(crate) fn max_output_tokens(model: &str) -> Option<u64> {
    profile(model).map(|_| MAX_OUTPUT_TOKENS)
}

/// Configuration for a Muse model served through Meta's Model API.
#[cfg(feature = "providers-compat")]
pub struct MuseConfig {
    /// API key sent as a bearer token.
    pub api_key: String,
    /// API base URL ending in the OpenAI-compatible version path, such as
    /// `https://api.meta.ai/v1`.
    pub base_url: String,
    /// Muse model identifier sent with each request.
    pub model: String,
}

#[cfg(test)]
#[cfg(feature = "providers-compat")]
#[path = "muse_test.rs"]
mod tests;
