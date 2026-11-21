//! Claude model identifiers, context profiles, and Anthropic API
//! environment for the built-in catalog.

use crate::model::{HeuristicContextEstimator, ModelProfile};

/// Base URL of the Anthropic API, ending in its version path.
pub(crate) const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1";
pub(crate) const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
pub(crate) const ANTHROPIC_BASE_URL_ENV: &str = "ANTHROPIC_BASE_URL";

/// Claude Fable 5.2, Anthropic's most capable widely released model.
pub(crate) const CLAUDE_FABLE_5_2: &str = "claude-fable-5-2";
/// Claude Opus 5.5, Anthropic's model for long-running agentic coding and
/// knowledge work.
pub(crate) const CLAUDE_OPUS_5_5: &str = "claude-opus-5-5";
/// Claude Sonnet 5.5, Anthropic's balance of speed and intelligence.
pub(crate) const CLAUDE_SONNET_5_5: &str = "claude-sonnet-5-5";
/// Claude Haiku 5.5, Anthropic's fastest model, for high-volume tasks.
pub(crate) const CLAUDE_HAIKU_5_5: &str = "claude-haiku-5-5";

/// Percent by which the unfitted default estimator may under-count Claude
/// tokens. No Claude model has a fitted estimator yet, so this takes the 50%
/// that keeps unknown tokenizers inside their window; calibration from
/// reported usage then narrows the prediction.
const ESTIMATE_UNDER_COUNT_PERCENT: u64 = 50;
/// Output limit of every cataloged model on the synchronous Messages API.
const MAX_OUTPUT_TOKENS: u64 = 128_000;
/// Every cataloged Claude model: documented 1,000,000-token window and
/// 128,000-token maximum output.
const CLAUDE_PROFILE: ModelProfile = ModelProfile::new(1_000_000)
    .with_estimate_margin(MAX_OUTPUT_TOKENS, ESTIMATE_UNDER_COUNT_PERCENT)
    .with_estimator(CLAUDE_ESTIMATOR);
/// Structured outputs add a system prompt explaining the output format, so
/// the output schema counts toward the request.
const CLAUDE_ESTIMATOR: HeuristicContextEstimator =
    HeuristicContextEstimator::DEFAULT.with_output_schema_in_prompt();

/// Returns the documented context profile of a cataloged Claude model, or
/// `None` for a model this catalog does not list.
pub(crate) fn profile(model: &str) -> Option<ModelProfile> {
    match model {
        CLAUDE_FABLE_5_2 | CLAUDE_OPUS_5_5 | CLAUDE_SONNET_5_5 | CLAUDE_HAIKU_5_5 => {
            Some(CLAUDE_PROFILE)
        }
        _ => None,
    }
}

/// Returns the documented output limit of a cataloged Claude model, or
/// `None` for a model this catalog does not list.
pub(crate) fn max_output_tokens(model: &str) -> Option<u64> {
    profile(model).map(|_| MAX_OUTPUT_TOKENS)
}

/// Returns the `max_tokens` a request to a cataloged Claude model sends
/// when it sets no output bound, or `None` for a model this catalog does not
/// list, which keeps the preset's 16,000.
///
/// Thinking counts toward `max_tokens`, so 16,000 can cut off an answer at
/// a high effort. Requests stream, so 64,000, the bound Anthropic suggests
/// for streamed requests, stays half of the documented 128,000-token
/// limit while leaving room for long reasoning.
#[cfg(feature = "providers-anthropic")]
pub(crate) fn fallback_max_tokens(model: &str) -> Option<std::num::NonZeroU32> {
    profile(model).map(|_| std::num::NonZeroU32::MIN.saturating_add(63_999))
}

#[cfg(test)]
#[path = "claude_test.rs"]
mod tests;
