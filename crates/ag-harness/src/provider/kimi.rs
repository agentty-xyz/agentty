//! Kimi model identifiers, context profiles, and configuration for Moonshot
//! AI's OpenAI-compatible API.

use crate::model::{HeuristicContextEstimator, ModelProfile};

pub(crate) const DEFAULT_BASE_URL: &str = "https://api.moonshot.ai/v1";
pub(crate) const KIMI_API_KEY_ENV: &str = "KIMI_API_KEY";
pub(crate) const KIMI_BASE_URL_ENV: &str = "KIMI_BASE_URL";

/// Kimi K3 model identifier.
pub const KIMI_K3: &str = "kimi-k3";

/// Kimi K2.6 model identifier.
pub const KIMI_K2_6: &str = "kimi-k2.6";

/// Percent by which the fitted Kimi estimators may under-count: the largest
/// under-count they showed on recorded requests and on a live request near
/// the input budget, rounded up, plus five points.
const ESTIMATE_UNDER_COUNT_PERCENT: u64 = 17;
/// Kimi K3's default `max_completion_tokens`.
const KIMI_K3_MAX_OUTPUT_TOKENS: u64 = 131_072;
/// Kimi K2.6's default `max_tokens`.
const KIMI_K2_6_MAX_OUTPUT_TOKENS: u64 = 32_768;
/// Kimi K3: documented 1M (1,048,576-token) window.
const KIMI_K3_PROFILE: ModelProfile = ModelProfile::new(1_048_576)
    .with_estimate_margin(KIMI_K3_MAX_OUTPUT_TOKENS, ESTIMATE_UNDER_COUNT_PERCENT)
    .with_estimator(KIMI_K3_ESTIMATOR);
/// Kimi K2.6: documented 262,144-token window, the limit its overflow
/// errors report.
const KIMI_K2_6_PROFILE: ModelProfile = ModelProfile::new(262_144)
    .with_estimate_margin(KIMI_K2_6_MAX_OUTPUT_TOKENS, ESTIMATE_UNDER_COUNT_PERCENT)
    .with_estimator(KIMI_K2_6_ESTIMATOR);
/// Kimi K3 in JSON Object mode, fitted to provider-reported input tokens:
/// content at 4.5 bytes per token with twice Kimi K2.6's message framing.
const KIMI_K3_ESTIMATOR: HeuristicContextEstimator = HeuristicContextEstimator::DEFAULT
    .with_messages(223, 54)
    .with_tool_definitions(307, 0)
    .with_request_tokens(55)
    .with_output_schema_in_prompt();
/// Kimi K2.6 in JSON Object mode, fitted to provider-reported input tokens:
/// content at 4.5 bytes per token, tool definitions at 3.2, and no
/// measurable fixed or tool-calling cost.
const KIMI_K2_6_ESTIMATOR: HeuristicContextEstimator = HeuristicContextEstimator::DEFAULT
    .with_messages(224, 25)
    .with_tool_definitions(312, 0)
    .with_request_tokens(0)
    .with_output_schema_in_prompt();

/// Returns the context profile of a cataloged Kimi model, or `None` for a
/// model this catalog does not list.
pub(crate) fn profile(model: &str) -> Option<ModelProfile> {
    match model {
        KIMI_K3 => Some(KIMI_K3_PROFILE),
        KIMI_K2_6 => Some(KIMI_K2_6_PROFILE),
        _ => None,
    }
}

/// Returns the output reservation of a cataloged Kimi model, or `None` for
/// a model this catalog does not list.
pub(crate) fn max_output_tokens(model: &str) -> Option<u64> {
    match model {
        KIMI_K3 => Some(KIMI_K3_MAX_OUTPUT_TOKENS),
        KIMI_K2_6 => Some(KIMI_K2_6_MAX_OUTPUT_TOKENS),
        _ => None,
    }
}

/// Configuration for a Kimi model served through Moonshot AI's
/// OpenAI-compatible API.
#[cfg(feature = "providers-compat")]
pub struct KimiConfig {
    /// API key sent as a bearer token.
    pub api_key: String,
    /// API base URL ending in the OpenAI-compatible version path, such as
    /// `https://api.moonshot.ai/v1`.
    pub base_url: String,
    /// Kimi model identifier sent with each request.
    pub model: String,
}

#[cfg(test)]
#[cfg(feature = "providers-compat")]
#[path = "kimi_test.rs"]
mod tests;
