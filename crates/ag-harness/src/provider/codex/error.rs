use std::sync::Arc;

use thiserror::Error;

use crate::model::{ModelError, ModelErrorType};

#[derive(Debug, Error)]
pub(super) enum CodexClientError {
    #[error("Codex authentication changed to a different ChatGPT account")]
    AuthAccountChanged,
    #[error(
        "Codex auth file is unavailable; set `CODEX_HOME` or configure `CodexConfig::auth_file`"
    )]
    AuthFileUnavailable,
    #[error("Codex auth path must name a regular file")]
    AuthFileNotRegular,
    #[error("Codex auth file exceeds the size limit")]
    AuthFileTooLarge,
    #[error("Codex authentication file task failed: {0}")]
    AuthFileTask(#[source] tokio::task::JoinError),
    #[error("ChatGPT login is required; authenticate Codex with ChatGPT first")]
    ChatGptLoginRequired,
    #[error("Codex response was incomplete: {reason}")]
    Incomplete { reason: String },
    #[error("failed to configure Codex HTTP client: {0}")]
    HttpClient(#[source] Arc<reqwest::Error>),
    #[error("Codex authentication contains an invalid header value")]
    InvalidAuthHeader,
    #[error("Codex authentication contains an invalid ID token")]
    InvalidIdToken,
    #[error("Codex session contains invalid Responses reasoning replay data")]
    InvalidReasoningReplay,
    #[error("Codex returned an invalid event stream: {reason}")]
    InvalidSse { reason: String },
    #[error("Codex authentication is missing `{0}`")]
    MissingAuthField(&'static str),
    #[error("Codex does not support harness tool definitions")]
    UnsupportedTools,
    #[error("Codex response is missing `{0}`")]
    MissingResponseField(&'static str),
    #[error("failed to parse Codex authentication: {0}")]
    ParseAuth(#[source] serde_json::Error),
    #[error("Codex request failed: {message}")]
    Provider { message: String },
    #[error("failed to read Codex authentication: {0}")]
    ReadAuth(#[source] std::io::Error),
    #[error("Codex response exceeds the size limit")]
    ResponseTooLarge,
    #[error("Codex response content exceeds the size limit")]
    ResponseContentTooLarge,
    #[error("Codex response exceeded the overall timeout")]
    ResponseTimeout,
    #[error("Codex request headers exceeded the timeout")]
    RequestTimeout,
    #[error("Codex response stream exceeded the idle timeout")]
    StreamIdleTimeout,
    #[error("Codex transport failed: {0}")]
    Transport(#[source] reqwest::Error),
}

impl CodexClientError {
    pub(super) fn into_model_error(self) -> ModelError {
        match self {
            Self::ResponseTooLarge => ModelError::ResponseBodyTooLarge,
            Self::ResponseContentTooLarge => ModelError::ResponseContentTooLarge,
            Self::Incomplete { reason } => ModelError::IncompleteResponse { reason },
            error @ (Self::InvalidSse { .. } | Self::MissingResponseField(_)) => {
                ModelError::classified_request(
                    ModelErrorType::InvalidProviderResponse,
                    None,
                    Box::new(error),
                )
            }
            error @ Self::Provider { .. } => {
                ModelError::classified_request(ModelErrorType::Provider, None, Box::new(error))
            }
            error @ Self::UnsupportedTools => ModelError::classified_request(
                ModelErrorType::UnsupportedCapability,
                None,
                Box::new(error),
            ),
            error @ (Self::HttpClient(_)
            | Self::RequestTimeout
            | Self::ResponseTimeout
            | Self::StreamIdleTimeout
            | Self::Transport(_)) => {
                ModelError::classified_request(ModelErrorType::Transport, None, Box::new(error))
            }
            error => ModelError::classified_request(ModelErrorType::Request, None, Box::new(error)),
        }
    }
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
