use super::*;

#[test]
fn test_agent_transport_app_server_uses_app_server() {
    // Arrange
    let transport = AgentTransport::AppServer;

    // Act
    let result = transport.uses_app_server();

    // Assert
    assert!(result);
}

#[test]
fn test_agent_transport_cli_does_not_use_app_server() {
    // Arrange
    let transport = AgentTransport::Cli;

    // Act
    let result = transport.uses_app_server();

    // Assert
    assert!(!result);
}

#[test]
fn test_agent_backend_error_setup_displays_message() {
    // Arrange
    let error = AgentBackendError::Setup("setup failed".to_string());

    // Act
    let display = format!("{error}");

    // Assert
    assert_eq!(display, "setup failed");
}

#[test]
fn test_agent_backend_error_command_build_displays_message() {
    // Arrange
    let error = AgentBackendError::CommandBuild("build failed".to_string());

    // Act
    let display = format!("{error}");

    // Assert
    assert_eq!(display, "build failed");
}

#[test]
fn test_agent_backend_error_implements_std_error() {
    // Arrange
    let error = AgentBackendError::Setup("test error".to_string());

    // Act
    let std_error: &dyn Error = &error;

    // Assert
    assert_eq!(std_error.to_string(), "test error");
    assert!(std_error.source().is_none());
}

#[test]
fn test_agent_backend_error_setup_and_command_build_are_distinct() {
    // Arrange
    let setup_error = AgentBackendError::Setup("failure".to_string());
    let build_error = AgentBackendError::CommandBuild("failure".to_string());

    // Act / Assert
    assert_ne!(setup_error, build_error);
}
