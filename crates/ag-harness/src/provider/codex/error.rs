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
                    Box::new(error),
                )
            }
            error @ Self::Provider { .. } => {
                ModelError::classified_request(ModelErrorType::Provider, Box::new(error))
            }
            error @ Self::UnsupportedTools => ModelError::classified_request(
                ModelErrorType::UnsupportedCapability,
                Box::new(error),
            ),
            error @ (Self::HttpClient(_) | Self::StreamIdleTimeout | Self::Transport(_)) => {
                ModelError::classified_request(ModelErrorType::Transport, Box::new(error))
            }
            error => ModelError::classified_request(ModelErrorType::Request, Box::new(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ModelError, ModelErrorType};

    #[test]
    fn client_errors_map_to_stable_model_error_types() {
        // Arrange
        let errors = [
            CodexClientError::ResponseTooLarge,
            CodexClientError::ResponseContentTooLarge,
            CodexClientError::Incomplete {
                reason: "limit".to_string(),
            },
            CodexClientError::InvalidSse {
                reason: "invalid".to_string(),
            },
            CodexClientError::MissingResponseField("field"),
            CodexClientError::Provider {
                message: "failed".to_string(),
            },
            CodexClientError::StreamIdleTimeout,
            CodexClientError::UnsupportedTools,
            CodexClientError::AuthFileUnavailable,
        ];

        // Act
        let mapped = errors.map(CodexClientError::into_model_error);

        // Assert
        assert!(matches!(mapped[0], ModelError::ResponseBodyTooLarge));
        assert!(matches!(mapped[1], ModelError::ResponseContentTooLarge));
        assert!(matches!(mapped[2], ModelError::IncompleteResponse { .. }));
        assert_eq!(
            mapped[3].error_type(),
            ModelErrorType::InvalidProviderResponse
        );
        assert_eq!(
            mapped[4].error_type(),
            ModelErrorType::InvalidProviderResponse
        );
        assert_eq!(mapped[5].error_type(), ModelErrorType::Provider);
        assert_eq!(mapped[6].error_type(), ModelErrorType::Transport);
        assert_eq!(
            mapped[7].error_type(),
            ModelErrorType::UnsupportedCapability
        );
        assert_eq!(mapped[8].error_type(), ModelErrorType::Request);
    }
}
