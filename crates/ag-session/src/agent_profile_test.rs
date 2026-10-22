use super::AGENT_PROFILE_PROMPT_TRUNCATION_MARKER;
use crate::agent_profile::{
    AGENT_PROFILE_PROMPT_MAX_BYTES, AgentDefinition, AgentDefinitionParseError,
    AgentDefinitionSummary, parse_agent_definition, parse_agent_summary,
};

#[test]
fn bundled_and_custom_definitions_share_the_parser_and_restrict_permissions() {
    // Arrange
    let source = "---\nid: investigator\nname: Investigator\ndescription: Investigates \
                  failures\nmodel: inherit\npermission-mode: read-only\n---\nInspect the supplied \
                  evidence.";

    // Act
    let custom = parse_agent_definition("fallback", source)
        .expect("valid definition")
        .expect("enabled");
    let bundled = crate::bundled_agent_definitions();

    // Assert
    assert_eq!(bundled.len(), 2);
    assert_eq!(bundled[0].id, "planner");
    assert_eq!(bundled[1].id, "implementer");
    assert!(crate::AgentDefinition::bundled("unknown").is_none());
    assert_eq!(
        custom.permission_mode(crate::PermissionMode::AutoEdit),
        crate::PermissionMode::ReadOnly
    );
    assert_eq!(
        bundled[1].permission_mode(crate::PermissionMode::ReadOnly),
        crate::PermissionMode::ReadOnly
    );
    assert_eq!(
        bundled[1].permission_mode(crate::PermissionMode::AutoEdit),
        crate::PermissionMode::AutoEdit
    );
    assert_eq!(
        custom.summary().source,
        crate::AgentDefinitionSource::Repository
    );
    let encoded = serde_json::to_string(&custom).expect("serialize definition");
    assert_eq!(
        serde_json::from_str::<AgentDefinition>(&encoded).expect("restore definition"),
        custom
    );
}

#[test]
fn catalog_identity_survives_disabled_definitions_and_execution_options_are_validated() {
    // Arrange
    let disabled = "---\nid: planner\nenabled: false\n---\n";
    let fields = [
        "model: sonnet",
        "permission-mode: bypass",
        "tools: Read",
        "disallowedTools: Write",
        "permissionMode: plan",
        "mcpServers: filesystem",
        "hooks: run",
        "skills: review",
        "connection-type: stdio",
        "isolation: worktree",
        "background: true",
        "maxTurns: 10",
        "unknown-setting: value",
        "model: inherit\nmodel: inherit",
        "permission-mode: inherit\npermission-mode: inherit",
    ];

    // Act / Assert
    assert_eq!(
        crate::agent_definition_identity("fallback", disabled).expect("disabled identity"),
        "planner"
    );
    assert_eq!(
        crate::agent_definition_identity("fallback", "---\nname: Test\n---\n")
            .expect("directory identity"),
        "fallback"
    );
    assert!(crate::agent_definition_identity("", "---\n---\n").is_err());
    assert!(crate::agent_definition_identity("test", "invalid").is_err());
    for field in fields {
        let source =
            format!("---\nname: Test\ndescription: Test agent\n{field}\n---\nInstructions");
        assert!(parse_agent_definition("test", &source).is_err(), "{field}");
        assert!(parse_agent_summary("test", &source).is_err(), "{field}");
    }
    assert!(
        parse_agent_definition(
            "test",
            "---\nname: Test\ndescription: Test agent\npermission-mode: inherit\n---\nInstructions"
        )
        .expect("inherited permissions")
        .is_some()
    );
}

#[test]
fn test_parse_agent_definition_reads_enabled_profile() {
    // Arrange
    let definition = r#"---
id: reviewer
name: "Code Reviewer"
description: 'Reviews code carefully'

role: delegation-target
connection-type: internal
enabled: true
---

Focus on correctness and security.
"#;

    // Act
    let agent_profile = parse_agent_definition("fallback", definition)
        .expect("definition should parse")
        .expect("definition should be enabled");

    // Assert
    assert_eq!(
        agent_profile,
        AgentDefinition {
            read_only: false,
            description: "Reviews code carefully".to_string(),
            id: "reviewer".to_string(),
            name: "Code Reviewer".to_string(),
            prompt: "Focus on correctness and security.".to_string(),
        }
    );
}

#[test]
fn test_parse_agent_summary_returns_metadata_without_prompt_body() {
    // Arrange
    let definition = "---\nid: reviewer\nname: Reviewer\ndescription: Reviews code\n---\nA very \
                      large prompt body.";

    // Act
    let summary = parse_agent_summary("fallback", definition)
        .expect("definition should parse")
        .expect("definition should be enabled");

    // Assert
    assert_eq!(
        summary,
        AgentDefinitionSummary {
            source: crate::AgentDefinitionSource::Repository,
            description: "Reviews code".to_string(),
            id: "reviewer".to_string(),
            name: "Reviewer".to_string(),
        }
    );
}

#[test]
fn test_parse_agent_definition_uses_directory_id_when_id_is_missing() {
    // Arrange
    let definition = "---\nname: Planner\ndescription: Plans work\nenabled: true\n---\nPlan first.";

    // Act
    let agent_profile = parse_agent_definition("planner", definition)
        .expect("definition should parse")
        .expect("definition should be enabled");

    // Assert
    assert_eq!(agent_profile.id, "planner");
}

#[test]
fn test_parse_agent_definition_omits_disabled_profile() {
    // Arrange
    let definition = "---\nname: Disabled\ndescription: Hidden\nenabled: false\n---\nDo not load.";

    // Act
    let agent_profile =
        parse_agent_definition("disabled", definition).expect("definition should parse");

    // Assert
    assert_eq!(agent_profile, None);
}

#[test]
fn test_parse_agent_definition_rejects_malformed_frontmatter() {
    // Arrange
    let definition = "---\nname Reviewer\n---\nReview code.";

    // Act
    let error = parse_agent_definition("reviewer", definition)
        .expect_err("malformed frontmatter should fail");

    // Assert
    assert!(matches!(
        error,
        AgentDefinitionParseError::InvalidFrontmatter(_)
    ));
}

#[test]
fn test_parse_agent_definition_rejects_invalid_enabled_value() {
    // Arrange
    let definition =
        "---\nname: Reviewer\ndescription: Reviews code\nenabled: yes\n---\nReview code.";

    // Act
    let error = parse_agent_definition("reviewer", definition)
        .expect_err("invalid enabled value should fail");

    // Assert
    assert!(matches!(
        error,
        AgentDefinitionParseError::InvalidFrontmatter(_)
    ));
}

#[test]
fn test_parse_agent_definition_supports_quoted_values_and_crlf() {
    // Arrange
    let definition = "---\r\nname: 'Strict Reviewer'\r\ndescription: \"Reviews code: \
                      carefully\"\r\nenabled: true\r\n---\r\nReview carefully.\r\n";

    // Act
    let agent_profile = parse_agent_definition("reviewer", definition)
        .expect("frontmatter definition should parse")
        .expect("definition should be enabled");

    // Assert
    assert_eq!(agent_profile.name, "Strict Reviewer");
    assert_eq!(agent_profile.description, "Reviews code: carefully");
    assert_eq!(agent_profile.prompt, "Review carefully.");
}

#[test]
fn test_parse_agent_definition_rejects_invalid_simple_frontmatter_fields() {
    // Arrange
    let duplicate = "---\nname: Reviewer\nname: Other\ndescription: Reviews code\n---\nReview.";
    let duplicate_enabled = "---\nname: Reviewer\ndescription: Reviews code\nenabled: \
                             true\nenabled: false\n---\nReview.";
    let empty_key = "---\n: value\nname: Reviewer\ndescription: Reviews code\n---\nReview.";
    let unterminated = "---\nname: \"Reviewer\ndescription: Reviews code\n---\nReview.";

    // Act
    let duplicate_error = parse_agent_definition("reviewer", duplicate)
        .expect_err("duplicate supported field should fail");
    let duplicate_enabled_error = parse_agent_definition("reviewer", duplicate_enabled)
        .expect_err("duplicate enabled field should fail");
    let empty_key_error =
        parse_agent_definition("reviewer", empty_key).expect_err("empty key should fail");
    let unterminated_error = parse_agent_definition("reviewer", unterminated)
        .expect_err("unterminated quoted value should fail");

    // Assert
    assert!(matches!(
        duplicate_error,
        AgentDefinitionParseError::InvalidFrontmatter(_)
    ));
    assert!(matches!(
        duplicate_enabled_error,
        AgentDefinitionParseError::InvalidFrontmatter(_)
    ));
    assert!(matches!(
        empty_key_error,
        AgentDefinitionParseError::InvalidFrontmatter(_)
    ));
    assert!(matches!(
        unterminated_error,
        AgentDefinitionParseError::InvalidFrontmatter(_)
    ));
}

#[test]
fn test_parse_agent_definition_rejects_missing_frontmatter_delimiter() {
    // Arrange
    let missing_open = "name: Reviewer\n---\nReview.";
    let missing_close = "---\nname: Reviewer\nReview.";

    // Act
    let missing_open_error = parse_agent_definition("reviewer", missing_open)
        .expect_err("opening delimiter should be required");
    let missing_close_error = parse_agent_definition("reviewer", missing_close)
        .expect_err("closing delimiter should be required");

    // Assert
    assert_eq!(
        missing_open_error,
        AgentDefinitionParseError::MissingFrontmatter
    );
    assert_eq!(
        missing_close_error,
        AgentDefinitionParseError::MissingFrontmatter
    );
}

#[test]
fn test_parse_agent_definition_requires_description_and_prompt() {
    // Arrange
    let missing_description = "---\nname: Reviewer\n---\nReview code.";
    let missing_prompt = "---\nname: Reviewer\ndescription: Reviews code\n---\n";

    // Act
    let description_error = parse_agent_definition("reviewer", missing_description)
        .expect_err("missing description should fail");
    let prompt_error =
        parse_agent_definition("reviewer", missing_prompt).expect_err("missing prompt should fail");
    let summary_prompt_error = parse_agent_summary("reviewer", missing_prompt)
        .expect_err("summary should require a prompt");

    // Assert
    assert_eq!(
        description_error,
        AgentDefinitionParseError::MissingField("description")
    );
    assert_eq!(
        prompt_error,
        AgentDefinitionParseError::MissingField("prompt")
    );
    assert_eq!(
        summary_prompt_error,
        AgentDefinitionParseError::MissingField("prompt")
    );
}

#[test]
fn test_parse_agent_definition_truncates_large_prompt_at_utf8_boundary() {
    // Arrange
    let prompt = "é".repeat(AGENT_PROFILE_PROMPT_MAX_BYTES);
    let definition = format!("---\nname: Large\ndescription: Large prompt\n---\n{prompt}");

    // Act
    let agent_profile = parse_agent_definition("large", &definition)
        .expect("definition should parse")
        .expect("definition should be enabled");

    // Assert
    assert!(agent_profile.prompt.len() <= AGENT_PROFILE_PROMPT_MAX_BYTES);
    assert!(
        agent_profile
            .prompt
            .ends_with(AGENT_PROFILE_PROMPT_TRUNCATION_MARKER)
    );
    assert!(
        agent_profile
            .prompt
            .is_char_boundary(agent_profile.prompt.len())
    );
}
