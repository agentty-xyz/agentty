use super::*;

#[test]
/// Ensures transport capability is provided by infra backend descriptors,
/// not domain enums.
fn test_transport_mode_reports_expected_transport_by_provider() {
    // Arrange
    let antigravity_kind = AgentKind::Antigravity;
    let claude_kind = AgentKind::Claude;
    let codex_kind = AgentKind::Codex;
    let gemini_kind = AgentKind::Gemini;

    // Act
    let antigravity_transport = transport_mode(antigravity_kind);
    let claude_transport = transport_mode(claude_kind);
    let codex_transport = transport_mode(codex_kind);
    let gemini_transport = transport_mode(gemini_kind);

    // Assert
    assert_eq!(antigravity_transport, AgentTransport::AppServer);
    assert_eq!(claude_transport, AgentTransport::Cli);
    assert_eq!(codex_transport, AgentTransport::AppServer);
    assert_eq!(gemini_transport, AgentTransport::AppServer);
}

#[test]
/// Ensures prompt delivery is also derived from the shared provider
/// descriptor.
fn test_prompt_transport_reports_expected_mode_by_provider() {
    // Arrange
    let antigravity_kind = AgentKind::Antigravity;
    let claude_kind = AgentKind::Claude;
    let codex_kind = AgentKind::Codex;
    let gemini_kind = AgentKind::Gemini;

    // Act
    let antigravity_transport = prompt_transport(antigravity_kind);
    let claude_transport = prompt_transport(claude_kind);
    let codex_transport = prompt_transport(codex_kind);
    let gemini_transport = prompt_transport(gemini_kind);

    // Assert
    assert_eq!(antigravity_transport, AgentPromptTransport::Argv);
    assert_eq!(claude_transport, AgentPromptTransport::Stdin);
    assert_eq!(codex_transport, AgentPromptTransport::Argv);
    assert_eq!(gemini_transport, AgentPromptTransport::Argv);
}

#[test]
/// Ensures provider schema capabilities are derived from the shared
/// descriptor.
fn test_protocol_schema_instruction_mode_reports_expected_mode_by_provider() {
    // Arrange / Act / Assert
    assert_eq!(
        protocol_schema_instruction_mode(AgentKind::Antigravity),
        ProtocolSchemaInstructionMode::TransportSchema
    );
    assert_eq!(
        protocol_schema_instruction_mode(AgentKind::Gemini),
        ProtocolSchemaInstructionMode::PromptSchema
    );
    assert_eq!(
        protocol_schema_instruction_mode(AgentKind::Claude),
        ProtocolSchemaInstructionMode::TransportSchema
    );
    assert_eq!(
        protocol_schema_instruction_mode(AgentKind::Codex),
        ProtocolSchemaInstructionMode::TransportSchema
    );
}

#[test]
/// Ensures providers reject malformed final protocol payloads.
fn test_parse_turn_response_rejects_invalid_payload() {
    // Arrange
    let raw_response = "plain response";

    for kind in [
        AgentKind::Antigravity,
        AgentKind::Claude,
        AgentKind::Codex,
        AgentKind::Gemini,
    ] {
        // Act
        let error = parse_turn_response(kind, raw_response, ProtocolRequestProfile::SessionTurn)
            .expect_err("plain response should fail strict protocol parsing");

        // Assert
        assert!(error.contains("debug_details:"));
        assert!(error.contains("first_non_whitespace_char: 'p'"));
        assert!(error.contains("direct_json_error_location: line 1, column 1"));
    }
}

#[test]
/// Ensures Codex app-server phase labels map to thought deltas through the
/// shared provider descriptor.
fn test_is_app_server_thought_chunk_reports_codex_phase_labels() {
    // Arrange / Act / Assert
    assert!(is_app_server_thought_chunk(
        AgentKind::Codex,
        true,
        Some("thinking"),
    ));
    assert!(!is_app_server_thought_chunk(
        AgentKind::Gemini,
        true,
        Some("thinking"),
    ));
}
