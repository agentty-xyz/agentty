//! `ChatGPT` plan provider: model identifiers, context profiles, and sign-in
//! credential file lookup.

#[cfg(feature = "providers-openai")]
use std::env;
#[cfg(feature = "providers-openai")]
use std::path::{Path, PathBuf};

#[cfg(feature = "providers-openai")]
use ag_router::chatgpt::ChatGptCredentials;
use thiserror::Error;

use crate::model::{HeuristicContextEstimator, ModelProfile};

pub(crate) const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
/// Environment variable naming the `ChatGPT` credential file when the host
/// does not pass one.
pub const CHATGPT_AUTH_FILE_ENV: &str = "CHATGPT_AUTH_FILE";
pub(crate) const CHATGPT_BASE_URL_ENV: &str = "CHATGPT_BASE_URL";

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
#[cfg(feature = "providers-openai")]
pub struct ChatGptConfig {
    /// Credential file of the sign-in, such as the one
    /// [`super::ChatGptSignIn`] writes.
    pub auth_file: PathBuf,
    /// API base URL ending in the version path, such as
    /// `https://api.openai.com/v1`.
    pub base_url: String,
    /// Model identifier sent with each request.
    pub model: String,
}

/// Credential file location could not be determined.
#[derive(Debug, Error)]
#[error("set CHATGPT_AUTH_FILE or pass the ChatGPT credential file to locate the sign-in")]
pub struct AuthFileUnavailable;

/// `ChatGPT` credential file relative to a host's data directory.
pub const CHATGPT_CREDENTIAL_FILE: &str = "auth/chatgpt.json";

/// Returns the `ChatGPT` credential file `CHATGPT_AUTH_FILE` names, the
/// fallback when the host passes no file.
///
/// # Errors
///
/// Returns [`AuthFileUnavailable`] when the variable is unset, blank, or
/// not valid Unicode.
#[cfg(feature = "providers-openai")]
pub(crate) fn auth_file(
    mut environment: impl FnMut(&str) -> Result<String, env::VarError>,
) -> Result<PathBuf, AuthFileUnavailable> {
    environment(CHATGPT_AUTH_FILE_ENV)
        .ok()
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
        .ok_or(AuthFileUnavailable)
}

/// Returns the `ChatGPT` credential file of a host whose data directory is
/// `data_dir`: the file `CHATGPT_AUTH_FILE` names when it is set and not
/// blank, else [`CHATGPT_CREDENTIAL_FILE`] under `data_dir`, or `None` when
/// neither is available.
///
/// The harness keeps no default data directory; each host resolves its
/// own, such as Agentty's `~/.agentty`, and passes the result to
/// [`super::ModelConfiguration::auth_file`] and
/// [`super::ChatGptSignIn::new`]. Hosts that share a data directory, such as
/// Agentty and `ag-harness login chatgpt`, thereby find the same sign-in.
#[cfg(feature = "providers-openai")]
pub fn auth_file_in(
    mut environment: impl FnMut(&str) -> Result<String, env::VarError>,
    data_dir: Option<&Path>,
) -> Option<PathBuf> {
    auth_file(&mut environment)
        .ok()
        .or_else(|| data_dir.map(|data_dir| data_dir.join(CHATGPT_CREDENTIAL_FILE)))
}

/// Returns whether `auth_file` holds a sign-in that can still serve
/// requests: it exists, is readable, and `OpenAI` has not rejected it.
#[cfg(feature = "providers-openai")]
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

/// Returns the output limit of a cataloged plan model, or `None` for a
/// model this catalog does not list.
pub(crate) fn max_output_tokens(model: &str) -> Option<u64> {
    profile(model).map(|_| MAX_OUTPUT_TOKENS)
}

#[cfg(test)]
#[cfg(feature = "providers-openai")]
#[path = "chatgpt_test.rs"]
mod tests;
