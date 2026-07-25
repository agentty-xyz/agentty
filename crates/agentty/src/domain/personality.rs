//! Workspace personality definitions parsed from `.agents/agents/*/agent.md`.

/// Maximum UTF-8 byte length retained for one personality prompt.
pub const PERSONALITY_PROMPT_MAX_BYTES: usize = 16 * 1024;

/// FNV-1a domain separator for the stable personality fingerprint encoding.
const PERSONALITY_FINGERPRINT_DOMAIN: &[u8] = b"agentty-personality-v1";

/// FNV-1a 64-bit offset basis.
const FNV_1A_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a 64-bit prime.
const FNV_1A_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Marker appended when a personality prompt exceeds the supported budget.
const PERSONALITY_PROMPT_TRUNCATION_MARKER: &str = "\n\n[Personality prompt truncated at 16 KiB.]";

/// One enabled personality loaded from a workspace agent definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Personality {
    /// Short explanation shown beside the personality name.
    pub description: String,
    /// Stable identifier persisted on the owning session.
    pub id: String,
    /// Human-readable picker label.
    pub name: String,
    /// Behavioral preamble injected into agent turns.
    pub prompt: String,
}

impl Personality {
    /// Returns lightweight picker metadata without retaining the prompt body.
    #[must_use]
    pub fn summary(&self) -> PersonalitySummary {
        PersonalitySummary {
            description: self.description.clone(),
            id: self.id.clone(),
            name: self.name.clone(),
        }
    }

    /// Returns a deterministic fingerprint for the selected ID and prompt.
    ///
    /// Fingerprints only detect changes; they are not used for cryptographic
    /// verification. The versioned FNV-1a encoding is stable across processes,
    /// platforms, and Rust toolchain upgrades.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let mut fingerprint =
            update_fnv_1a_fingerprint(FNV_1A_OFFSET_BASIS, PERSONALITY_FINGERPRINT_DOMAIN);
        for component in [&self.id, &self.prompt] {
            let byte_length = (component.len() as u128).to_le_bytes();
            fingerprint = update_fnv_1a_fingerprint(fingerprint, &byte_length);
            fingerprint = update_fnv_1a_fingerprint(fingerprint, component.as_bytes());
        }

        format!("{fingerprint:016x}")
    }
}

/// Updates one FNV-1a fingerprint with the supplied bytes.
fn update_fnv_1a_fingerprint(mut fingerprint: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        fingerprint ^= u64::from(*byte);
        fingerprint = fingerprint.wrapping_mul(FNV_1A_PRIME);
    }

    fingerprint
}

/// Lightweight personality metadata stored in prompt-composer state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PersonalitySummary {
    /// Short explanation shown beside the personality name.
    pub description: String,
    /// Stable identifier persisted on the owning session.
    pub id: String,
    /// Human-readable picker label.
    pub name: String,
}

/// Supported fields decoded from one `.agents` frontmatter block.
#[derive(Default)]
struct AgentFrontmatter {
    description: Option<String>,
    enabled: Option<bool>,
    id: Option<String>,
    name: Option<String>,
}

/// Error returned when one agent definition cannot be parsed safely.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PersonalityParseError {
    /// The definition does not contain a complete frontmatter block.
    #[error("missing or incomplete frontmatter")]
    MissingFrontmatter,
    /// The simple `key: value` frontmatter is malformed.
    #[error("invalid frontmatter: {0}")]
    InvalidFrontmatter(String),
    /// A required frontmatter value is absent or blank.
    #[error("missing required `{0}` frontmatter value")]
    MissingField(&'static str),
}

/// Parses one `.agents` agent definition.
///
/// `directory_id` is used when the optional frontmatter `id` is absent.
/// Disabled definitions return `Ok(None)` so callers can omit them without
/// treating intentional configuration as a parse failure.
///
/// # Errors
/// Returns [`PersonalityParseError`] when frontmatter is malformed or a
/// required name, description, directory fallback ID, or prompt is missing.
pub fn parse_agent_definition(
    directory_id: &str,
    contents: &str,
) -> Result<Option<Personality>, PersonalityParseError> {
    let (frontmatter, body) = split_agent_definition(contents)?;
    let frontmatter = parse_agent_frontmatter(frontmatter)?;
    if frontmatter.enabled == Some(false) {
        return Ok(None);
    }

    let id = required_value(frontmatter.id.as_deref().unwrap_or(directory_id), "id")?;
    let name = required_value(frontmatter.name.as_deref().unwrap_or_default(), "name")?;
    let description = required_value(
        frontmatter.description.as_deref().unwrap_or_default(),
        "description",
    )?;
    let prompt = required_value(body, "prompt")?;

    Ok(Some(Personality {
        description: description.to_string(),
        id: id.to_string(),
        name: name.to_string(),
        prompt: truncate_personality_prompt(prompt),
    }))
}

/// Splits an agent definition into simple frontmatter and Markdown body.
fn split_agent_definition(contents: &str) -> Result<(&str, &str), PersonalityParseError> {
    let mut lines = contents.split_inclusive('\n');
    let first_line = lines
        .next()
        .ok_or(PersonalityParseError::MissingFrontmatter)?;
    if first_line.trim() != "---" {
        return Err(PersonalityParseError::MissingFrontmatter);
    }
    let frontmatter_start = first_line.len();
    let mut line_start = frontmatter_start;

    for line in lines {
        if line.trim() == "---" {
            let body_start = line_start.saturating_add(line.len());

            return Ok((
                &contents[frontmatter_start..line_start],
                &contents[body_start..],
            ));
        }
        line_start = line_start.saturating_add(line.len());
    }

    Err(PersonalityParseError::MissingFrontmatter)
}

/// Parses the protocol's simple line-oriented `key: value` frontmatter.
fn parse_agent_frontmatter(frontmatter: &str) -> Result<AgentFrontmatter, PersonalityParseError> {
    let mut parsed = AgentFrontmatter::default();

    for (line_index, line) in frontmatter.lines().enumerate() {
        let line_number = line_index.saturating_add(1);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(invalid_frontmatter_line(
                line_number,
                "expected `key: value`",
            ));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(invalid_frontmatter_line(line_number, "key is empty"));
        }
        let value = parse_frontmatter_value(value.trim(), line_number)?;

        match key {
            "description" => {
                set_frontmatter_string(&mut parsed.description, key, value, line_number)?;
            }
            "enabled" => {
                if parsed.enabled.is_some() {
                    return Err(invalid_frontmatter_line(
                        line_number,
                        "duplicate `enabled` field",
                    ));
                }
                parsed.enabled = Some(match value {
                    "true" => true,
                    "false" => false,
                    _ => {
                        return Err(invalid_frontmatter_line(
                            line_number,
                            "`enabled` must be `true` or `false`",
                        ));
                    }
                });
            }
            "id" => {
                set_frontmatter_string(&mut parsed.id, key, value, line_number)?;
            }
            "name" => {
                set_frontmatter_string(&mut parsed.name, key, value, line_number)?;
            }
            _ => {}
        }
    }

    Ok(parsed)
}

/// Builds one line-numbered frontmatter parsing error.
fn invalid_frontmatter_line(line_number: usize, message: &str) -> PersonalityParseError {
    PersonalityParseError::InvalidFrontmatter(format!("line {line_number}: {message}"))
}

/// Removes matching single or double quotes from one frontmatter value.
fn parse_frontmatter_value(value: &str, line_number: usize) -> Result<&str, PersonalityParseError> {
    let Some(quote) = value
        .chars()
        .next()
        .filter(|quote| matches!(quote, '\'' | '"'))
    else {
        return Ok(value);
    };
    if value.len() < 2 || !value.ends_with(quote) {
        return Err(invalid_frontmatter_line(
            line_number,
            "quoted value is not terminated",
        ));
    }

    Ok(&value[quote.len_utf8()..value.len().saturating_sub(quote.len_utf8())])
}

/// Assigns one supported string field and rejects duplicates.
fn set_frontmatter_string(
    target: &mut Option<String>,
    key: &str,
    value: &str,
    line_number: usize,
) -> Result<(), PersonalityParseError> {
    if target.is_some() {
        return Err(invalid_frontmatter_line(
            line_number,
            &format!("duplicate `{key}` field"),
        ));
    }
    *target = Some(value.to_string());

    Ok(())
}

/// Returns one non-empty required value.
fn required_value<'a>(
    value: &'a str,
    field: &'static str,
) -> Result<&'a str, PersonalityParseError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(PersonalityParseError::MissingField(field));
    }

    Ok(value)
}

/// Truncates a prompt at a UTF-8 boundary while retaining the marker budget.
fn truncate_personality_prompt(prompt: &str) -> String {
    if prompt.len() <= PERSONALITY_PROMPT_MAX_BYTES {
        return prompt.to_string();
    }

    let content_budget =
        PERSONALITY_PROMPT_MAX_BYTES.saturating_sub(PERSONALITY_PROMPT_TRUNCATION_MARKER.len());
    let mut boundary = content_budget.min(prompt.len());
    while !prompt.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }

    format!(
        "{}{}",
        prompt[..boundary].trim_end(),
        PERSONALITY_PROMPT_TRUNCATION_MARKER
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_parse_agent_definition_uses_directory_id_when_id_is_missing() {
        // Arrange
        let definition =
            "---\nname: Planner\ndescription: Plans work\nenabled: true\n---\nPlan first.";

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
        let definition =
            "---\nname: Disabled\ndescription: Hidden\nenabled: false\n---\nDo not load.";

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
        let prompt_error = parse_agent_definition("reviewer", missing_prompt)
            .expect_err("missing prompt should fail");

        // Assert
        assert_eq!(
            description_error,
            PersonalityParseError::MissingField("description")
        );
        assert_eq!(prompt_error, PersonalityParseError::MissingField("prompt"));
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
}
