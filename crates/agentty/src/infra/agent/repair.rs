//! Protocol-repair retry for malformed agent responses.
//!
//! When an agent response fails strict JSON schema validation, this module
//! builds a repair prompt that tells the agent to re-emit its response as
//! valid protocol JSON. Each transport channel (`cli.rs`, `app_server.rs`,
//! `submission.rs`) uses [`build_protocol_repair_prompt`] to construct the
//! prompt and handles the retry in its own transport-specific way.

use askama::Template;

use super::backend::AgentBackendError;
use super::protocol;

/// Maximum characters of the malformed response included in the repair
/// prompt preview so the agent can see what it produced without flooding
/// the context window.
const REPAIR_RESPONSE_PREVIEW_MAX_CHARS: usize = 500;

/// Askama view model for the protocol repair prompt template.
///
/// The template includes the `Structured response protocol:` marker so
/// [`super::prompt::prepend_protocol_instructions`] detects it and skips
/// double-wrapping when the repair prompt flows through the standard
/// prompt preparation pipeline.
#[derive(Template)]
#[template(path = "protocol_repair_prompt.md", escape = "none")]
struct ProtocolRepairPromptTemplate<'a> {
    /// Human-readable parse error from the failed attempt.
    parse_error: &'a str,
    /// Pretty-printed JSON schema contract the agent must satisfy.
    response_json_schema: &'a str,
    /// Truncated preview of the malformed response.
    response_preview: &'a str,
}

/// Builds the protocol repair prompt text for one failed parse attempt.
///
/// The returned prompt is self-contained: it includes the full JSON schema
/// and the `Structured response protocol:` marker so it can be submitted
/// through the standard prompt pipeline without being double-wrapped.
///
/// # Errors
/// Returns an error when the Askama template fails to render.
pub(crate) fn build_protocol_repair_prompt(
    parse_error: &str,
    malformed_response: &str,
) -> Result<String, AgentBackendError> {
    let response_json_schema = protocol::agent_response_json_schema_json();
    let response_preview = truncate_preview(malformed_response, REPAIR_RESPONSE_PREVIEW_MAX_CHARS);
    let template = ProtocolRepairPromptTemplate {
        parse_error,
        response_json_schema: &response_json_schema,
        response_preview: &response_preview,
    };

    template.render().map_err(|error| {
        AgentBackendError::CommandBuild(format!(
            "Failed to render `protocol_repair_prompt.md`: {error}"
        ))
    })
}

/// Truncates a raw response to a character-limited preview for the repair
/// prompt.
fn truncate_preview(raw: &str, max_chars: usize) -> String {
    let preview: String = raw.chars().take(max_chars).collect();
    let total_chars = raw.chars().count();

    if total_chars <= max_chars {
        return preview;
    }

    format!("{preview}\n... [{} more chars]", total_chars - max_chars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Repair prompt renders with the parse error and a response preview.
    fn test_build_protocol_repair_prompt_includes_error_and_preview() {
        // Arrange
        let parse_error = "response is not valid protocol JSON: invalid JSON";
        let malformed_response = "plain text response";

        // Act
        let repair_prompt = build_protocol_repair_prompt(parse_error, malformed_response)
            .expect("repair prompt should render");

        // Assert
        assert!(repair_prompt.contains(parse_error));
        assert!(repair_prompt.contains("plain text response"));
        assert!(repair_prompt.contains("Structured response protocol:"));
        assert!(repair_prompt.contains("Authoritative JSON Schema:"));
        assert!(repair_prompt.contains("\"answer\""));
    }

    #[test]
    /// Repair prompt truncates long malformed responses to the preview limit.
    fn test_build_protocol_repair_prompt_truncates_long_response() {
        // Arrange
        let parse_error = "schema validation failed";
        let malformed_response = "x".repeat(1000);

        // Act
        let repair_prompt = build_protocol_repair_prompt(parse_error, &malformed_response)
            .expect("repair prompt should render");

        // Assert
        assert!(repair_prompt.contains("500 more chars"));
        assert!(!repair_prompt.contains(&malformed_response));
    }

    #[test]
    /// Repair prompt includes the protocol marker to prevent double-wrapping.
    fn test_build_protocol_repair_prompt_contains_protocol_marker() {
        // Arrange / Act
        let repair_prompt =
            build_protocol_repair_prompt("error", "response").expect("repair prompt should render");

        // Assert
        assert!(repair_prompt.contains("Structured response protocol:"));
    }

    #[test]
    /// Short responses are not truncated.
    fn test_truncate_preview_keeps_short_responses_intact() {
        // Arrange / Act
        let preview = truncate_preview("short", 500);

        // Assert
        assert_eq!(preview, "short");
    }

    #[test]
    /// Long responses are truncated with a character count suffix.
    fn test_truncate_preview_truncates_long_responses() {
        // Arrange
        let long_response = "a".repeat(600);

        // Act
        let preview = truncate_preview(&long_response, 500);

        // Assert
        assert!(preview.starts_with(&"a".repeat(500)));
        assert!(preview.contains("100 more chars"));
    }
}
