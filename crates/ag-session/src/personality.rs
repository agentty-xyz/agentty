//! Workspace personality definitions parsed from `.agents/agents/*/agent.md`.

/// Maximum UTF-8 byte length retained for one personality prompt.
pub const PERSONALITY_PROMPT_MAX_BYTES: usize = 16 * 1024;

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
            Self::update_fnv_1a_fingerprint(FNV_1A_OFFSET_BASIS, PERSONALITY_FINGERPRINT_DOMAIN);
        for component in [&self.id, &self.prompt] {
            let byte_length = (component.len() as u128).to_le_bytes();
            fingerprint = Self::update_fnv_1a_fingerprint(fingerprint, &byte_length);
            fingerprint = Self::update_fnv_1a_fingerprint(fingerprint, component.as_bytes());
        }

        format!("{fingerprint:016x}")
    }

    /// Updates one FNV-1a fingerprint with the supplied bytes.
    fn update_fnv_1a_fingerprint(mut fingerprint: u64, bytes: &[u8]) -> u64 {
        for byte in bytes {
            fingerprint ^= u64::from(*byte);
            fingerprint = fingerprint.wrapping_mul(FNV_1A_PRIME);
        }

        fingerprint
    }
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
    let Some(summary) = parse_agent_summary_parts(directory_id, frontmatter)? else {
        return Ok(None);
    };

    let prompt = required_value(body, "prompt")?;

    Ok(Some(Personality {
        description: summary.description,
        id: summary.id,
        name: summary.name,
        prompt: truncate_personality_prompt(prompt),
    }))
}

/// Parses lightweight picker metadata from one `.agents` agent definition.
///
/// Callers may supply a body reduced to any non-empty placeholder because the
/// returned value does not retain prompt text. Disabled definitions return
/// `Ok(None)`.
///
/// # Errors
/// Returns [`PersonalityParseError`] under the same validation rules as
/// [`parse_agent_definition`].
pub fn parse_agent_summary(
    directory_id: &str,
    contents: &str,
) -> Result<Option<PersonalitySummary>, PersonalityParseError> {
    let (frontmatter, body) = split_agent_definition(contents)?;
    let Some(summary) = parse_agent_summary_parts(directory_id, frontmatter)? else {
        return Ok(None);
    };
    required_value(body, "prompt")?;

    Ok(Some(summary))
}

/// FNV-1a domain separator for the stable personality fingerprint encoding.
const PERSONALITY_FINGERPRINT_DOMAIN: &[u8] = b"agentty-personality-v1";

/// FNV-1a 64-bit offset basis.
const FNV_1A_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a 64-bit prime.
const FNV_1A_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Marker appended when a personality prompt exceeds the supported budget.
const PERSONALITY_PROMPT_TRUNCATION_MARKER: &str = "\n\n[Personality prompt truncated at 16 KiB.]";

/// Supported fields decoded from one `.agents` frontmatter block.
#[derive(Default)]
struct AgentFrontmatter {
    description: Option<String>,
    enabled: Option<bool>,
    id: Option<String>,
    name: Option<String>,
}

impl AgentFrontmatter {
    /// Parses the protocol's simple line-oriented `key: value` frontmatter.
    fn parse(frontmatter: &str) -> Result<Self, PersonalityParseError> {
        let mut parsed = Self::default();

        for (line_index, line) in frontmatter.lines().enumerate() {
            let line_number = line_index.saturating_add(1);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let Some((key, value)) = line.split_once(':') else {
                return Err(Self::invalid_frontmatter_line(
                    line_number,
                    "expected `key: value`",
                ));
            };
            let key = key.trim();
            if key.is_empty() {
                return Err(Self::invalid_frontmatter_line(line_number, "key is empty"));
            }

            let value = Self::parse_frontmatter_value(value.trim(), line_number)?;

            match key {
                "description" => {
                    Self::set_frontmatter_string(&mut parsed.description, key, value, line_number)?;
                }
                "enabled" => {
                    if parsed.enabled.is_some() {
                        return Err(Self::invalid_frontmatter_line(
                            line_number,
                            "duplicate `enabled` field",
                        ));
                    }
                    parsed.enabled = Some(match value {
                        "true" => true,
                        "false" => false,
                        _ => {
                            return Err(Self::invalid_frontmatter_line(
                                line_number,
                                "`enabled` must be `true` or `false`",
                            ));
                        }
                    });
                }
                "id" => {
                    Self::set_frontmatter_string(&mut parsed.id, key, value, line_number)?;
                }
                "name" => {
                    Self::set_frontmatter_string(&mut parsed.name, key, value, line_number)?;
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
    fn parse_frontmatter_value(
        value: &str,
        line_number: usize,
    ) -> Result<&str, PersonalityParseError> {
        let Some(quote) = value
            .chars()
            .next()
            .filter(|quote| matches!(quote, '\'' | '"'))
        else {
            return Ok(value);
        };
        if value.len() < 2 || !value.ends_with(quote) {
            return Err(Self::invalid_frontmatter_line(
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
            return Err(Self::invalid_frontmatter_line(
                line_number,
                &format!("duplicate `{key}` field"),
            ));
        }

        *target = Some(value.to_string());

        Ok(())
    }
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

/// Validates frontmatter and builds lightweight picker metadata.
fn parse_agent_summary_parts(
    directory_id: &str,
    frontmatter: &str,
) -> Result<Option<PersonalitySummary>, PersonalityParseError> {
    let frontmatter = AgentFrontmatter::parse(frontmatter)?;
    if frontmatter.enabled == Some(false) {
        return Ok(None);
    }

    let id = required_value(frontmatter.id.as_deref().unwrap_or(directory_id), "id")?;
    let name = required_value(frontmatter.name.as_deref().unwrap_or_default(), "name")?;
    let description = required_value(
        frontmatter.description.as_deref().unwrap_or_default(),
        "description",
    )?;

    Ok(Some(PersonalitySummary {
        description: description.to_string(),
        id: id.to_string(),
        name: name.to_string(),
    }))
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
#[path = "personality_test.rs"]
mod tests;
