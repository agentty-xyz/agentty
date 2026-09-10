use super::PERSONALITY_PROMPT_TRUNCATION_MARKER;
use crate::personality::{
    PERSONALITY_PROMPT_MAX_BYTES, Personality, PersonalityParseError, PersonalitySummary,
    parse_agent_definition, parse_agent_summary,
};

#[test]
fn test_parse_agent_definition_reads_enabled_profile() {
    // Arrange
    let definition = r#"---
id: reviewer
name: "Code Reviewer"
description: 'Reviews code carefully'

role: delegation-target
enabled: true
---

Focus on correctness and security.
"#;

    // Act
    let personality = parse_agent_definition("fallback", definition)
        .expect("definition should parse")
        .expect("definition should be enabled");

    // Assert
    assert_eq!(
        personality,
        Personality {
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
        PersonalitySummary {
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
    let personality = parse_agent_definition("planner", definition)
        .expect("definition should parse")
        .expect("definition should be enabled");

    // Assert
    assert_eq!(personality.id, "planner");
}

#[test]
fn test_parse_agent_definition_omits_disabled_profile() {
    // Arrange
    let definition = "---\nname: Disabled\ndescription: Hidden\nenabled: false\n---\nDo not load.";

    // Act
    let personality =
        parse_agent_definition("disabled", definition).expect("definition should parse");

    // Assert
    assert_eq!(personality, None);
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
        PersonalityParseError::InvalidFrontmatter(_)
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
        PersonalityParseError::InvalidFrontmatter(_)
    ));
}

#[test]
fn test_parse_agent_definition_supports_quoted_values_and_crlf() {
    // Arrange
    let definition = "---\r\nname: 'Strict Reviewer'\r\ndescription: \"Reviews code: \
                      carefully\"\r\nenabled: true\r\n---\r\nReview carefully.\r\n";

    // Act
    let personality = parse_agent_definition("reviewer", definition)
        .expect("frontmatter definition should parse")
        .expect("definition should be enabled");

    // Assert
    assert_eq!(personality.name, "Strict Reviewer");
    assert_eq!(personality.description, "Reviews code: carefully");
    assert_eq!(personality.prompt, "Review carefully.");
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
        PersonalityParseError::InvalidFrontmatter(_)
    ));
    assert!(matches!(
        duplicate_enabled_error,
        PersonalityParseError::InvalidFrontmatter(_)
    ));
    assert!(matches!(
        empty_key_error,
        PersonalityParseError::InvalidFrontmatter(_)
    ));
    assert!(matches!(
        unterminated_error,
        PersonalityParseError::InvalidFrontmatter(_)
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
        PersonalityParseError::MissingFrontmatter
    );
    assert_eq!(
        missing_close_error,
        PersonalityParseError::MissingFrontmatter
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
        PersonalityParseError::MissingField("description")
    );
    assert_eq!(prompt_error, PersonalityParseError::MissingField("prompt"));
    assert_eq!(
        summary_prompt_error,
        PersonalityParseError::MissingField("prompt")
    );
}

#[test]
fn test_parse_agent_definition_truncates_large_prompt_at_utf8_boundary() {
    // Arrange
    let prompt = "é".repeat(PERSONALITY_PROMPT_MAX_BYTES);
    let definition = format!("---\nname: Large\ndescription: Large prompt\n---\n{prompt}");

    // Act
    let personality = parse_agent_definition("large", &definition)
        .expect("definition should parse")
        .expect("definition should be enabled");

    // Assert
    assert!(personality.prompt.len() <= PERSONALITY_PROMPT_MAX_BYTES);
    assert!(
        personality
            .prompt
            .ends_with(PERSONALITY_PROMPT_TRUNCATION_MARKER)
    );
    assert!(
        personality
            .prompt
            .is_char_boundary(personality.prompt.len())
    );
}

#[test]
fn test_personality_fingerprint_changes_with_id_or_prompt() {
    // Arrange
    let personality = Personality {
        description: "Reviews code".to_string(),
        id: "reviewer".to_string(),
        name: "Reviewer".to_string(),
        prompt: "Review carefully.".to_string(),
    };
    let mut changed_id = personality.clone();
    changed_id.id = "security-reviewer".to_string();
    let mut changed_prompt = personality.clone();
    changed_prompt.prompt = "Review security carefully.".to_string();

    // Act
    let fingerprint = personality.fingerprint();

    // Assert
    assert_eq!(fingerprint, "dad9785239f763e7");
    assert_ne!(fingerprint, changed_id.fingerprint());
    assert_ne!(fingerprint, changed_prompt.fingerprint());
}

#[test]
fn test_personality_fingerprint_separates_id_and_prompt_components() {
    // Arrange
    let first = Personality {
        description: String::new(),
        id: "ab".to_string(),
        name: String::new(),
        prompt: "c".to_string(),
    };
    let second = Personality {
        description: String::new(),
        id: "a".to_string(),
        name: String::new(),
        prompt: "bc".to_string(),
    };

    // Act
    let first_fingerprint = first.fingerprint();
    let second_fingerprint = second.fingerprint();

    // Assert
    assert_ne!(first_fingerprint, second_fingerprint);
}
