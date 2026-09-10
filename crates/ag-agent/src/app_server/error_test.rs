use crate::app_server::error::AppServerError;
use crate::app_server_transport::AppServerTransportError;

#[test]
fn lock_poisoned_display_includes_provider_name() {
    // Arrange
    let error = AppServerError::LockPoisoned { provider: "Codex" };

    // Act
    let display = error.to_string();

    // Assert
    assert_eq!(display, "Failed to lock Codex app-server session map");
}

#[test]
fn interrupted_by_user_display_shows_message() {
    // Arrange
    let error = AppServerError::InterruptedByUser("[Stopped]".to_string());

    // Act / Assert
    assert_eq!(error.to_string(), "[Stopped]");
}

#[test]
fn prompt_render_display_shows_message() {
    // Arrange
    let error = AppServerError::PromptRender("template syntax error".to_string());

    // Act / Assert
    assert_eq!(error.to_string(), "template syntax error");
}

#[test]
fn provider_display_shows_message() {
    // Arrange
    let error = AppServerError::Provider("runtime crashed".to_string());

    // Act / Assert
    assert_eq!(error.to_string(), "runtime crashed");
}

#[test]
fn retry_exhausted_display_includes_both_errors() {
    // Arrange
    let error = AppServerError::RetryExhausted {
        provider: "Codex",
        first_error: "connection reset".to_string(),
        retry_error: "timeout".to_string(),
    };

    // Act
    let display = error.to_string();

    // Assert
    assert_eq!(
        display,
        "Codex app-server failed, then retry failed after restart: first error: connection reset; \
         retry error: timeout"
    );
}

#[test]
fn transport_display_delegates_to_inner_error() {
    // Arrange
    let error = AppServerError::from(AppServerTransportError::ProcessTerminated);

    // Act / Assert
    assert_eq!(
        error.to_string(),
        "App-server terminated before sending expected response"
    );
}

#[test]
fn transport_from_conversion_wraps_transport_error() {
    // Arrange
    let transport_error = AppServerTransportError::Timeout {
        response_id: "init-1".to_string(),
        timeout_seconds: 300,
    };

    // Act
    let error: AppServerError = transport_error.into();

    // Assert
    assert!(matches!(error, AppServerError::Transport(_)));
}
