//! Anthropic Messages API configuration.

/// Base URL of the Anthropic API, ending in its version path.
pub const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com/v1";

/// Configuration for a Claude model served through the Anthropic Messages
/// API by [`super::ProviderSpec::anthropic`].
pub struct AnthropicConfig {
    /// API key sent in the `x-api-key` header.
    pub api_key: String,
    /// API base URL ending in the version path, such as
    /// [`DEFAULT_ANTHROPIC_BASE_URL`].
    pub base_url: String,
    /// Claude model identifier sent with each request, such as
    /// `claude-opus-5-5`.
    pub model: String,
}
