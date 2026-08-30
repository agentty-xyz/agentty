use super::CodexClientError;
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
