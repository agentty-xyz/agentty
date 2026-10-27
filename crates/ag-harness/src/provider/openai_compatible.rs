use std::env;

use ag_router::LocalServer;

use super::catalog::ModelConfigurationError;
use crate::model::{HeuristicContextEstimator, ModelProfile};

pub(crate) const DEFAULT_BASE_URL: &str = "http://127.0.0.1:11434/v1";
pub(crate) const OPENAI_COMPATIBLE_API_KEY_ENV: &str = "OPENAI_COMPATIBLE_API_KEY";
pub(crate) const OPENAI_COMPATIBLE_BASE_URL_ENV: &str = "OPENAI_COMPATIBLE_BASE_URL";
/// Environment variable declaring the served model's context window in
/// tokens.
pub const OPENAI_COMPATIBLE_CONTEXT_WINDOW_ENV: &str = "OPENAI_COMPATIBLE_CONTEXT_WINDOW";
/// Environment variable declaring the tokens reserved for one response.
pub const OPENAI_COMPATIBLE_MAX_OUTPUT_TOKENS_ENV: &str = "OPENAI_COMPATIBLE_MAX_OUTPUT_TOKENS";

/// Context window assumed when none is declared: 32,768 tokens. A server
/// must run the model with at least this window; Ollama, for one, loads
/// models with a 4,096-token default and silently truncates longer input.
pub const DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = 32_768;
/// Response reservation assumed when none is declared.
pub const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 8_192;
/// Percent by which the unfitted default estimator may under-count the
/// tokens of an unknown tokenizer. The fitted hosted models under-count by at
/// most 18% under their own estimators, and the default estimator ran up to
/// 46% under on dense content, so 50% keeps unknown models inside the window.
const ESTIMATE_UNDER_COUNT_PERCENT: u64 = 50;

/// Configuration for any server implementing the `OpenAI` Chat Completions
/// API, such as Ollama, llama.cpp, LM Studio, or vLLM.
pub struct OpenAiCompatibleConfig {
    /// Optional API key sent as a bearer token; local servers usually need
    /// none.
    pub api_key: Option<String>,
    /// API base URL ending in the OpenAI-compatible version path, such as
    /// Ollama's `http://127.0.0.1:11434/v1`.
    pub base_url: String,
    /// Model identifier the server serves, such as `qwen3:latest`.
    pub model: String,
}

/// Returns the declared profile of an OpenAI-compatible model from
/// [`OPENAI_COMPATIBLE_CONTEXT_WINDOW_ENV`] and
/// [`OPENAI_COMPATIBLE_MAX_OUTPUT_TOKENS_ENV`], defaulting to
/// [`DEFAULT_CONTEXT_WINDOW_TOKENS`] and [`DEFAULT_MAX_OUTPUT_TOKENS`].
///
/// Unknown models have no fitted estimator, so requests are weighed by
/// [`HeuristicContextEstimator::DEFAULT`], counting the output schema the
/// provider states in the prompt, under a 50% estimate margin.
pub(crate) fn profile_from_environment(
    environment: impl FnMut(&str) -> Result<String, env::VarError>,
) -> Result<ModelProfile, ModelConfigurationError> {
    profile(environment, None)
}

/// Returns the profile of an OpenAI-compatible model the server runs with a
/// `running_context_window`-token window. A declared window larger than the
/// running one is capped to it, so requests never exceed what the server
/// keeps; without a declared reservation, a quarter of the window, at most
/// [`DEFAULT_MAX_OUTPUT_TOKENS`], is reserved for the response.
pub(crate) fn running_profile(
    environment: impl FnMut(&str) -> Result<String, env::VarError>,
    running_context_window: u64,
) -> Result<ModelProfile, ModelConfigurationError> {
    profile(environment, Some(running_context_window))
}

/// Builds the profile from declared counts and the server's running window.
fn profile(
    mut environment: impl FnMut(&str) -> Result<String, env::VarError>,
    running_context_window: Option<u64>,
) -> Result<ModelProfile, ModelConfigurationError> {
    let declared_context_window =
        token_count(&mut environment, OPENAI_COMPATIBLE_CONTEXT_WINDOW_ENV)?;
    let context_window = match (declared_context_window, running_context_window) {
        (Some(declared), Some(running)) => declared.min(running),
        (declared, running) => declared
            .or(running)
            .unwrap_or(DEFAULT_CONTEXT_WINDOW_TOKENS),
    };
    let max_output_tokens = token_count(&mut environment, OPENAI_COMPATIBLE_MAX_OUTPUT_TOKENS_ENV)?
        .unwrap_or_else(|| DEFAULT_MAX_OUTPUT_TOKENS.min(context_window / 4));
    if max_output_tokens >= context_window {
        return Err(ModelConfigurationError::OutputExceedsContextWindow {
            context_window,
            max_output_tokens,
        });
    }

    Ok(ModelProfile::new(context_window)
        .with_estimate_margin(max_output_tokens, ESTIMATE_UNDER_COUNT_PERCENT)
        .with_estimator(HeuristicContextEstimator::DEFAULT.with_output_schema_in_prompt()))
}

/// Reads a positive token count from `name`, or `None` when it is unset or
/// blank.
fn token_count(
    environment: &mut impl FnMut(&str) -> Result<String, env::VarError>,
    name: &'static str,
) -> Result<Option<u64>, ModelConfigurationError> {
    let value = match environment(name) {
        Ok(value) if value.trim().is_empty() => return Ok(None),
        Ok(value) => value,
        Err(env::VarError::NotPresent) => return Ok(None),
        Err(source) => return Err(ModelConfigurationError::Environment { name, source }),
    };

    value
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|count| *count > 0)
        .map(Some)
        .ok_or(ModelConfigurationError::InvalidTokenCount { name, value })
}

/// Returns a client for the OpenAI-compatible server at
/// [`OPENAI_COMPATIBLE_BASE_URL_ENV`], defaulting to Ollama's local
/// endpoint, with the optional [`OPENAI_COMPATIBLE_API_KEY_ENV`] key.
pub(crate) fn local_server(
    mut environment: impl FnMut(&str) -> Result<String, env::VarError>,
) -> Result<LocalServer, ModelConfigurationError> {
    let base_url = match environment(OPENAI_COMPATIBLE_BASE_URL_ENV) {
        Ok(base_url) if !base_url.trim().is_empty() => base_url.trim().to_string(),
        Ok(_) | Err(env::VarError::NotPresent) => DEFAULT_BASE_URL.to_string(),
        Err(source) => {
            return Err(ModelConfigurationError::Environment {
                name: OPENAI_COMPATIBLE_BASE_URL_ENV,
                source,
            });
        }
    };

    Ok(LocalServer::new(
        base_url,
        environment(OPENAI_COMPATIBLE_API_KEY_ENV).ok(),
    ))
}

#[cfg(test)]
#[path = "openai_compatible_test.rs"]
mod tests;
