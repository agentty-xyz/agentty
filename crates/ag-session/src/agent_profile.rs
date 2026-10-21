//! Portable agent definitions shared by bundled and repository catalogs.

use serde::{Deserialize, Serialize};

use crate::PermissionMode;

/// Maximum UTF-8 byte length retained for one agent prompt.
pub const AGENT_PROFILE_PROMPT_MAX_BYTES: usize = 16 * 1024;

/// One enabled agent loaded from a workspace agent definition.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentDefinition {
    /// Short explanation shown beside the agent name.
    pub description: String,
    /// Stable identifier used to resolve the definition.
    pub id: String,
    /// Human-readable picker label.
    pub name: String,
    /// Behavioral preamble injected into agent turns.
    pub prompt: String,
    /// Additional access restriction, never a grant of session permissions.
    #[serde(default)]
    pub read_only: bool,
}

impl AgentDefinition {
    /// Resolves a bundled definition using the same parser as repository files.
    #[must_use]
    pub fn bundled(id: &str) -> Option<Self> {
        bundled_agent_definitions()
            .into_iter()
            .find(|agent| agent.id == id)
    }

    /// Intersects the definition's restriction with the host permission mode.
    #[must_use]
    pub fn permission_mode(&self, session_mode: PermissionMode) -> PermissionMode {
        if self.read_only {
            PermissionMode::ReadOnly
        } else {
            session_mode
        }
    }

    /// Returns catalog metadata without retaining the prompt body.
    #[must_use]
    pub fn summary(&self) -> AgentDefinitionSummary {
        AgentDefinitionSummary {
            description: self.description.clone(),
            id: self.id.clone(),
            name: self.name.clone(),
            source: AgentDefinitionSource::Repository,
        }
    }
}

/// Lightweight agent metadata returned by the definition catalog.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentDefinitionSummary {
    /// Short explanation shown beside the agent name.
    pub description: String,
    /// Stable identifier persisted on the owning session.
    pub id: String,
    /// Human-readable picker label.
    pub name: String,
    /// Origin shown in the agent picker, including repository overrides.
    pub source: AgentDefinitionSource,
}

/// Location from which a catalog resolved a definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentDefinitionSource {
    /// Default distributed with Agentty.
    Bundled,
    /// Definition in the session repository.
    Repository,
}

/// Reads an identity even for a disabled definition, which masks a bundled ID.
///
/// # Errors
/// Returns an error for malformed frontmatter or an empty identity.
pub fn agent_definition_identity(
    directory_id: &str,
    source: &str,
) -> Result<String, AgentDefinitionParseError> {
    let (frontmatter, _) = split_agent_definition(source)?;
    let parsed = AgentFrontmatter::parse(frontmatter)?;

    Ok(required_value(parsed.id.as_deref().unwrap_or(directory_id), "id")?.to_string())
}

/// Error returned when one agent definition cannot be parsed safely.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AgentDefinitionParseError {
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
/// Returns [`AgentDefinitionParseError`] when frontmatter is malformed or a
/// required name, description, directory fallback ID, or prompt is missing.
pub fn parse_agent_definition(
    directory_id: &str,
    contents: &str,
) -> Result<Option<AgentDefinition>, AgentDefinitionParseError> {
    let (frontmatter, body) = split_agent_definition(contents)?;
    let Some(summary) = parse_agent_summary_parts(directory_id, frontmatter)? else {
        return Ok(None);
    };
    let options = AgentFrontmatter::parse(frontmatter)?;
    let read_only = options.permission_mode.as_deref() == Some("read-only");

    let prompt = required_value(body, "prompt")?;

    Ok(Some(AgentDefinition {
        description: summary.description,
        id: summary.id,
        name: summary.name,
        prompt: truncate_agent_profile_prompt(prompt),
        read_only,
    }))
}

/// Bundled agents are available even when a repository has no `.agents`
/// directory.
#[must_use]
pub fn bundled_agent_definitions() -> Vec<AgentDefinition> {
    [
        ("planner", include_str!("agents/planner/agent.md")),
        ("implementer", include_str!("agents/implementer/agent.md")),
    ]
    .into_iter()
    .filter_map(|(id, source)| parse_agent_definition(id, source).ok().flatten())
    .collect()
}

/// Parses lightweight picker metadata from one `.agents` agent definition.
///
/// Callers may supply a body reduced to any non-empty placeholder because the
/// returned value does not retain prompt text. Disabled definitions return
/// `Ok(None)`.
///
/// # Errors
/// Returns [`AgentDefinitionParseError`] under the same validation rules as
/// [`parse_agent_definition`].
pub fn parse_agent_summary(
    directory_id: &str,
    contents: &str,
) -> Result<Option<AgentDefinitionSummary>, AgentDefinitionParseError> {
    let (frontmatter, body) = split_agent_definition(contents)?;
    let Some(summary) = parse_agent_summary_parts(directory_id, frontmatter)? else {
        return Ok(None);
    };
    required_value(body, "prompt")?;

    Ok(Some(summary))
}

/// Marker appended when an agent prompt exceeds the supported budget.
const AGENT_PROFILE_PROMPT_TRUNCATION_MARKER: &str =
    "\n\n[AgentDefinition prompt truncated at 16 KiB.]";

/// Supported fields decoded from one `.agents` frontmatter block.
#[derive(Default)]
struct AgentFrontmatter {
    description: Option<String>,
    enabled: Option<bool>,
    id: Option<String>,
    model: Option<String>,
    name: Option<String>,
    permission_mode: Option<String>,
}

impl AgentFrontmatter {
    /// Validates executable options identically for summaries and full bodies.
    fn validate_options(&self) -> Result<(), AgentDefinitionParseError> {
        if self
            .model
            .as_deref()
            .is_some_and(|model| model != "inherit")
        {
            return Err(AgentDefinitionParseError::InvalidFrontmatter(
                "only model: inherit is supported".to_string(),
            ));
        }
        if self
            .permission_mode
            .as_deref()
            .is_some_and(|mode| !matches!(mode, "inherit" | "read-only"))
        {
            return Err(AgentDefinitionParseError::InvalidFrontmatter(
                "permission-mode must be inherit or read-only".to_string(),
            ));
        }

        Ok(())
    }

    /// Parses the protocol's simple line-oriented `key: value` frontmatter.
    fn parse(frontmatter: &str) -> Result<Self, AgentDefinitionParseError> {
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
                "model" => {
                    Self::set_frontmatter_string(&mut parsed.model, key, value, line_number)?;
                }
                "permission-mode" => {
                    Self::set_frontmatter_string(
                        &mut parsed.permission_mode,
                        key,
                        value,
                        line_number,
                    )?;
                }
                "role" | "tags" => {}
                "connection-type" if value == "internal" => {}
                _ => {
                    return Err(Self::invalid_frontmatter_line(
                        line_number,
                        &format!("unsupported execution field `{key}`"),
                    ));
                }
            }
        }

        parsed.validate_options()?;

        Ok(parsed)
    }

    /// Builds one line-numbered frontmatter parsing error.
    fn invalid_frontmatter_line(line_number: usize, message: &str) -> AgentDefinitionParseError {
        AgentDefinitionParseError::InvalidFrontmatter(format!("line {line_number}: {message}"))
    }

    /// Removes matching single or double quotes from one frontmatter value.
    fn parse_frontmatter_value(
        value: &str,
        line_number: usize,
    ) -> Result<&str, AgentDefinitionParseError> {
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
    ) -> Result<(), AgentDefinitionParseError> {
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
fn split_agent_definition(contents: &str) -> Result<(&str, &str), AgentDefinitionParseError> {
    let mut lines = contents.split_inclusive('\n');
    let first_line = lines
        .next()
        .ok_or(AgentDefinitionParseError::MissingFrontmatter)?;
    if first_line.trim() != "---" {
        return Err(AgentDefinitionParseError::MissingFrontmatter);
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

    Err(AgentDefinitionParseError::MissingFrontmatter)
}

/// Validates frontmatter and builds lightweight picker metadata.
fn parse_agent_summary_parts(
    directory_id: &str,
    frontmatter: &str,
) -> Result<Option<AgentDefinitionSummary>, AgentDefinitionParseError> {
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

    Ok(Some(AgentDefinitionSummary {
        description: description.to_string(),
        id: id.to_string(),
        name: name.to_string(),
        source: AgentDefinitionSource::Repository,
    }))
}

/// Returns one non-empty required value.
fn required_value<'a>(
    value: &'a str,
    field: &'static str,
) -> Result<&'a str, AgentDefinitionParseError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AgentDefinitionParseError::MissingField(field));
    }

    Ok(value)
}

/// Truncates a prompt at a UTF-8 boundary while retaining the marker budget.
fn truncate_agent_profile_prompt(prompt: &str) -> String {
    if prompt.len() <= AGENT_PROFILE_PROMPT_MAX_BYTES {
        return prompt.to_string();
    }

    let content_budget =
        AGENT_PROFILE_PROMPT_MAX_BYTES.saturating_sub(AGENT_PROFILE_PROMPT_TRUNCATION_MARKER.len());
    let mut boundary = content_budget.min(prompt.len());
    while !prompt.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }

    format!(
        "{}{}",
        prompt[..boundary].trim_end(),
        AGENT_PROFILE_PROMPT_TRUNCATION_MARKER
    )
}

#[cfg(test)]
#[path = "agent_profile_test.rs"]
mod tests;
