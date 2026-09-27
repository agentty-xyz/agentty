pub(crate) const KIMI_API_KEY_ENV: &str = "KIMI_API_KEY";
pub(crate) const KIMI_BASE_URL_ENV: &str = "KIMI_BASE_URL";

/// Kimi K2.6 model identifier.
pub const KIMI_K2_6: &str = "kimi-k2.6";

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
