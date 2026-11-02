use std::env;
use std::path::{Path, PathBuf};

use ag_router::chatgpt::ChatGptCredentials;
use thiserror::Error;

use crate::model::{HeuristicContextEstimator, ModelProfile};

pub(crate) const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
/// Environment variable overriding the `ChatGPT` credential file.
pub const CHATGPT_AUTH_FILE_ENV: &str = "CHATGPT_AUTH_FILE";
pub(crate) const CHATGPT_BASE_URL_ENV: &str = "CHATGPT_BASE_URL";
/// Environment variable naming Agentty's data directory, `~/.agentty` by
/// default; the credential file is `auth/chatgpt.json` inside it.
const AGENTTY_ROOT_ENV: &str = "AGENTTY_ROOT";
const CREDENTIAL_FILE: &str = "auth/chatgpt.json";

/// GPT-6.1 Sol on a `ChatGPT` plan.
pub const CHATGPT_GPT_6_1_SOL: &str = "gpt-6.1-sol";
/// GPT-6.1 Terra on a `ChatGPT` plan.
pub const CHATGPT_GPT_6_1_TERRA: &str = "gpt-6.1-terra";
/// GPT-6 Astra on a `ChatGPT` plan.
pub const CHATGPT_GPT_6_ASTRA: &str = "gpt-6-astra";
/// GPT-6 Sol on a `ChatGPT` plan.
pub const CHATGPT_GPT_6_SOL: &str = "gpt-6-sol";
/// GPT-6 Luna on a `ChatGPT` plan, the fastest model for small tasks.
pub const CHATGPT_GPT_6_LUNA: &str = "gpt-6-luna";
/// GPT-5.6 Sol on a `ChatGPT` plan.
pub const CHATGPT_GPT_5_6_SOL: &str = "gpt-5.6-sol";
/// GPT-5.6 Terra on a `ChatGPT` plan.
pub const CHATGPT_GPT_5_6_TERRA: &str = "gpt-5.6-terra";
/// GPT-5.6 Luna on a `ChatGPT` plan.
pub const CHATGPT_GPT_5_6_LUNA: &str = "gpt-5.6-luna";

/// Percent by which the default estimator may under-count `OpenAI` tokens.
const ESTIMATE_UNDER_COUNT_PERCENT: u64 = 50;
/// Output limit of every cataloged model.
const MAX_OUTPUT_TOKENS: u64 = 128_000;
/// The Responses API renders the `json_schema` text format into the prompt.
const ESTIMATOR: HeuristicContextEstimator =
    HeuristicContextEstimator::DEFAULT.with_output_schema_in_prompt();
/// GPT-6.1 models: 500,000-token window, as the plan's model list reports.
const GPT_6_1_PROFILE: ModelProfile = ModelProfile::new(500_000)
    .with_estimate_margin(MAX_OUTPUT_TOKENS, ESTIMATE_UNDER_COUNT_PERCENT)
    .with_estimator(ESTIMATOR);
/// Other plan models: 272,000-token window, as the plan's model list reports.
const PLAN_PROFILE: ModelProfile = ModelProfile::new(272_000)
    .with_estimate_margin(MAX_OUTPUT_TOKENS, ESTIMATE_UNDER_COUNT_PERCENT)
    .with_estimator(ESTIMATOR);

/// Configuration for an `OpenAI` model billed to a `ChatGPT` plan.
pub struct ChatGptConfig {
    /// Credential file written by `ag-harness login chatgpt`.
    pub auth_file: PathBuf,
    /// API base URL ending in the version path, such as
    /// `https://api.openai.com/v1`.
    pub base_url: String,
    /// Model identifier sent with each request.
    pub model: String,
}

/// Credential file location could not be determined.
#[derive(Debug, Error)]
#[error("set CHATGPT_AUTH_FILE, AGENTTY_ROOT, or HOME to locate the ChatGPT sign-in")]
pub struct AuthFileUnavailable;

/// Returns the `ChatGPT` credential file: `CHATGPT_AUTH_FILE`, else
/// `auth/chatgpt.json` under `AGENTTY_ROOT` or `~/.agentty`.
///
/// # Errors
///
/// Returns [`AuthFileUnavailable`] when none of the variables is set.
pub fn auth_file(
    mut environment: impl FnMut(&str) -> Result<String, env::VarError>,
) -> Result<PathBuf, AuthFileUnavailable> {
    let non_empty =
        |value: Result<String, env::VarError>| value.ok().filter(|value| !value.is_empty());
    if let Some(path) = non_empty(environment(CHATGPT_AUTH_FILE_ENV)) {
        return Ok(PathBuf::from(path));
    }
    if let Some(root) = non_empty(environment(AGENTTY_ROOT_ENV)) {
        return Ok(PathBuf::from(root).join(CREDENTIAL_FILE));
    }
    let home = non_empty(environment("HOME")).ok_or(AuthFileUnavailable)?;

    Ok(PathBuf::from(home).join(".agentty").join(CREDENTIAL_FILE))
}

/// Returns whether `auth_file` holds a sign-in that can still serve
/// requests: it exists, is readable, and `OpenAI` has not rejected it.
pub fn is_signed_in(auth_file: &Path) -> bool {
    ChatGptCredentials::load(auth_file).is_ok_and(|credentials| credentials.is_signed_in())
}

/// Returns the context profile of a cataloged plan model, or `None` for a
/// model this catalog does not list.
pub(crate) fn profile(model: &str) -> Option<ModelProfile> {
    match model {
        CHATGPT_GPT_6_1_SOL | CHATGPT_GPT_6_1_TERRA => Some(GPT_6_1_PROFILE),
        CHATGPT_GPT_6_ASTRA
        | CHATGPT_GPT_6_SOL
        | CHATGPT_GPT_6_LUNA
        | CHATGPT_GPT_5_6_SOL
        | CHATGPT_GPT_5_6_TERRA
        | CHATGPT_GPT_5_6_LUNA => Some(PLAN_PROFILE),
        _ => None,
    }
}

#[cfg(test)]
#[path = "chatgpt_test.rs"]
mod tests;
