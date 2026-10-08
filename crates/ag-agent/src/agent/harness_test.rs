use std::sync::Arc;

use ag_contracts::{
    AgentRequestKind, ExecutionPolicy, PermissionMode, ReasoningLevel, SessionStats, SpeedMode,
};
use ag_harness::provider::ModelProvider;
use ag_session::AgentKind;
use tempfile::tempdir;

use crate::agent::backend::{AgentBackendError, AgentTransport, BuildCommandRequest};
use crate::agent::harness::HARNESS_UNAVAILABLE_MESSAGE;
use crate::agent::provider::{
    create_app_server_client, create_backend, parse_response, parse_stream_output_line,
    transport_mode,
};
use crate::app_server::{AppServerClient, MockAppServerClient};

#[test]
/// Verifies harness setup leaves the session workspace untouched.
fn test_harness_setup_creates_no_files() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let backend = create_backend(AgentKind::Harness);

    // Act
    backend
        .setup(temp_directory.path())
        .expect("setup should succeed");

    // Assert
    assert_eq!(
        std::fs::read_dir(temp_directory.path())
            .expect("failed to read dir")
            .count(),
        0
    );
}

#[test]
/// Verifies unconfigured harness turns fail before any process starts.
fn test_harness_build_command_fails_closed() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let backend = create_backend(AgentKind::Harness);

    // Act
    let result = backend.build_command(BuildCommandRequest {
        attachments: &[],
        execution_policy: &ExecutionPolicy::default(),
        folder: temp_directory.path(),
        main_checkout_root: None,
        model: "muse-spark-1.3",
        permission_mode: PermissionMode::AutoEdit,
        personality_prompt: None,
        prompt: "Summarize the repository",
        reasoning_level: ReasoningLevel::default(),
        replay_transcript: None,
        request_kind: &AgentRequestKind::SessionStart,
        speed_mode: SpeedMode::Normal,
    });

    // Assert
    assert_eq!(
        result.err(),
        Some(AgentBackendError::CommandBuild(
            HARNESS_UNAVAILABLE_MESSAGE.to_string()
        ))
    );
}

#[test]
/// Verifies the harness uses the native transport and registers no app-server
/// runtime, even when a default client is supplied.
fn test_harness_uses_no_app_server_client() {
    // Arrange
    let kind = AgentKind::Harness;
    let default_client: Arc<dyn AppServerClient> = Arc::new(MockAppServerClient::new());

    // Act
    let client = create_app_server_client(kind, Some(default_client));
    let transport = transport_mode(kind);

    // Assert
    assert!(client.is_none());
    assert_eq!(transport, AgentTransport::Native);
}

#[test]
/// Verifies harness output parsing yields no content because the harness
/// writes no CLI output.
fn test_harness_parsers_return_no_content() {
    // Arrange
    let output = "{\"answer\":\"ignored\"}";

    // Act
    let response = parse_response(AgentKind::Harness, output, "");
    let stream_line = parse_stream_output_line(AgentKind::Harness, output);

    // Assert
    assert_eq!(response.content, "");
    assert_eq!(response.stats, SessionStats::default());
    assert_eq!(stream_line, None);
}

#[test]
/// Verifies every Agentty harness model is a model the harness catalog knows.
fn test_harness_models_match_harness_catalog() {
    // Arrange
    let known_models = ModelProvider::all()
        .iter()
        .flat_map(|provider| provider.known_models())
        .copied()
        .collect::<Vec<_>>();

    // Act
    let unknown_models = AgentKind::Harness
        .models()
        .iter()
        .map(|model| model.as_str())
        .filter(|model_id| !known_models.contains(model_id))
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(unknown_models, Vec::<&str>::new());
}
