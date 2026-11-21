//! Anthropic Messages API model identifiers, context profiles, and
//! configuration.

use crate::model::{HeuristicContextEstimator, ModelProfile};

/// Base URL of the Anthropic API, ending in its version path.
pub const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com/v1";
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
pub(crate) const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
pub(crate) const ANTHROPIC_BASE_URL_ENV: &str = "ANTHROPIC_BASE_URL";

/// Claude Fable 5.2, Anthropic's most capable generally available model.
pub const CLAUDE_FABLE_5_2: &str = "claude-fable-5-2";

/// Claude Opus 5.5, the model Anthropic recommends for complex agentic
/// work.
pub const CLAUDE_OPUS_5_5: &str = "claude-opus-5-5";

/// Claude Sonnet 5.5, Anthropic's balanced model.
pub const CLAUDE_SONNET_5_5: &str = "claude-sonnet-5-5";

/// Claude Haiku 5.5, Anthropic's fastest model for lighter tasks.
pub const CLAUDE_HAIKU_5_5: &str = "claude-haiku-5-5";

/// Percent by which the unfitted default estimator may under-count Claude
/// tokens. No Claude model has a fitted estimator yet, so this takes the
/// 50% that keeps unknown tokenizers inside their window, which also covers
/// the roughly 30% more tokens the current Claude tokenizer produces than
/// earlier ones; calibration from reported usage then narrows the
/// prediction.
const ESTIMATE_UNDER_COUNT_PERCENT: u64 = 50;
/// Output limit of every cataloged model.
const MAX_OUTPUT_TOKENS: u64 = 128_000;
/// Every cataloged model: documented 1,000,000-token window and
/// 128,000-token maximum output.
const CLAUDE_5_FAMILY: ModelProfile = ModelProfile::new(1_000_000)
    .with_estimate_margin(MAX_OUTPUT_TOKENS, ESTIMATE_UNDER_COUNT_PERCENT)
    .with_estimator(ANTHROPIC_ESTIMATOR);
/// The Messages API adds a system prompt describing the `output_config`
/// format it constrains, so the output schema counts toward the request.
const ANTHROPIC_ESTIMATOR: HeuristicContextEstimator =
    HeuristicContextEstimator::DEFAULT.with_output_schema_in_prompt();

/// Configuration for a Claude model served through the Anthropic Messages
/// API by [`super::ProviderSpec::anthropic`].
#[cfg(feature = "providers-anthropic")]
pub struct AnthropicConfig {
    /// API key sent in the `x-api-key` header.
    pub api_key: String,
    /// API base URL ending in the version path, such as
    /// [`DEFAULT_ANTHROPIC_BASE_URL`].
    pub base_url: String,
    /// Claude model identifier sent with each request, such as
    /// [`CLAUDE_OPUS_5_5`].
    pub model: String,
}

/// Returns the documented context profile of a cataloged Claude model, or
/// `None` for a model this catalog does not list.
pub(crate) fn profile(model: &str) -> Option<ModelProfile> {
    match model {
        CLAUDE_FABLE_5_2 | CLAUDE_OPUS_5_5 | CLAUDE_SONNET_5_5 | CLAUDE_HAIKU_5_5 => {
            Some(CLAUDE_5_FAMILY)
        }
        _ => None,
    }
}

/// Returns the documented output limit of a cataloged Claude model, or
/// `None` for a model this catalog does not list.
pub(crate) fn max_output_tokens(model: &str) -> Option<u64> {
    profile(model).map(|_| MAX_OUTPUT_TOKENS)
}

#[cfg(test)]
#[cfg(feature = "providers-anthropic")]
#[path = "anthropic_test.rs"]
mod tests;
