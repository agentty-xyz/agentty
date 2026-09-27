pub(crate) const DASHSCOPE_API_KEY_ENV: &str = "DASHSCOPE_API_KEY";
pub(crate) const DASHSCOPE_BASE_URL_ENV: &str = "DASHSCOPE_BASE_URL";

/// Qwen Plus model identifier.
pub const QWEN_PLUS: &str = "qwen-plus";

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
